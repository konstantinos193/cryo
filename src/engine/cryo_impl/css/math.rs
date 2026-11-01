//! CSS math: `calc()`, `min()`, `max()` and `clamp()`.
//!
//! These are not a nicety on the modern web. A stylesheet from a
//! current toolchain reaches for them hundreds of times on one page —
//! `calc(var(--spacing) * 4)` for every margin, and
//! `clamp(32px, 4.2vw, 48px)` for every heading that has to look right
//! on a phone and a monitor. An engine that cannot read them loads the
//! rule, wins the cascade, and then drops the value: the heading comes
//! out at the inherited 14px and the page looks like unstyled text.
//!
//! Evaluation happens on *tokens*, before a property is parsed, the
//! same way [`super::custom`] substitutes `var()`. A math function is
//! replaced by the plain number or dimension it works out to, and
//! every property parser downstream sees an ordinary value and needs
//! to know nothing about any of this.
//!
//! The grammar is the usual one, and precedence matters:
//!
//! ```text
//!   sum     := product (('+' | '-') product)*
//!   product := value (('*' | '/') value)*
//!   value   := <number> | <dimension> | '(' sum ')' | <math function>
//! ```
//!
//! Known gaps, stated rather than hidden:
//! - **Percentages are not evaluated.** `calc(100% - 20px)` needs the
//!   length it is a percentage *of*, which is a layout question and
//!   not known here. Such a value is left alone rather than guessed
//!   at, so the declaration fails to parse and the property keeps its
//!   inherited or initial value.
//! - A unit mismatch (`calc(10px * 2px)`) yields nothing, as the spec
//!   says, rather than a wrong number.
//! - No `round()`, `mod()`, `rem()`, or trigonometry.

use super::parser::ComponentValue;
use super::tokenizer::CssToken;
use super::value::{Length, LengthContext, Unit};

/// How deep nesting may go. `calc(calc(calc(...)))` is legal and rare;
/// the bound exists so a pathological stylesheet cannot run away.
const MAX_DEPTH: u8 = 16;

/// What a math expression works out to.
///
/// The two cases stay apart because the result has to be written back
/// as a token, and `line-height: calc(1.2 * 1)` must not come back as
/// `1.2px`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    /// A length, already in pixels.
    Px(f64),
    /// A bare number, which has no unit and never gains one.
    Number(f64),
}

impl Value {
    fn as_token(self) -> ComponentValue {
        ComponentValue::Token(match self {
            Self::Px(value) => CssToken::Dimension {
                value,
                unit: "px".to_owned(),
            },
            Self::Number(value) => CssToken::Number {
                value,
                integer: value.fract() == 0.0,
            },
        })
    }

    fn number(self) -> f64 {
        match self {
            Self::Px(value) | Self::Number(value) => value,
        }
    }
}

/// Whether a function name is one of CSS's math functions.
fn is_math(name: &str) -> bool {
    ["calc", "min", "max", "clamp"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// Replace every math function in `values` with what it works out to.
///
/// A function that cannot be evaluated — a percentage inside it, an
/// unknown unit, a unit mismatch — is left exactly as it was, so the
/// property parser rejects it and the declaration is dropped. That is
/// the same end the spec reaches by calling such a value invalid.
pub fn evaluate_all(values: &[ComponentValue], context: &LengthContext) -> Vec<ComponentValue> {
    rewrite(values, context, 0)
}

fn rewrite(values: &[ComponentValue], context: &LengthContext, depth: u8) -> Vec<ComponentValue> {
    if depth >= MAX_DEPTH {
        return values.to_vec();
    }
    values
        .iter()
        .map(|value| match value {
            ComponentValue::Function { name, arguments } if is_math(name) => {
                match call(name, arguments, context, depth) {
                    Some(result) => result.as_token(),
                    None => value.clone(),
                }
            }
            // A math function may sit inside an ordinary one, as in
            // `rgb(0 0 0 / calc(1 / 2))`.
            ComponentValue::Function { name, arguments } => ComponentValue::Function {
                name: name.clone(),
                arguments: rewrite(arguments, context, depth + 1),
            },
            ComponentValue::Block { kind, values } => ComponentValue::Block {
                kind: *kind,
                values: rewrite(values, context, depth + 1),
            },
            token => token.clone(),
        })
        .collect()
}

/// One math function call.
fn call(
    name: &str,
    arguments: &[ComponentValue],
    context: &LengthContext,
    depth: u8,
) -> Option<Value> {
    let parts: Vec<Vec<ComponentValue>> = split_on_commas(arguments);
    let mut evaluated = Vec::with_capacity(parts.len());
    for part in &parts {
        evaluated.push(sum(&mut Cursor::new(part), context, depth)?);
    }

    if name.eq_ignore_ascii_case("calc") {
        return (evaluated.len() == 1).then(|| evaluated[0]);
    }
    if name.eq_ignore_ascii_case("clamp") {
        let [low, middle, high] = evaluated[..] else {
            return None;
        };
        return combine(low, middle.number().clamp(low.number(), high.number()));
    }
    // `min()` and `max()` take any number of arguments.
    let first = *evaluated.first()?;
    let pick = if name.eq_ignore_ascii_case("min") {
        evaluated
            .iter()
            .map(|v| v.number())
            .fold(f64::INFINITY, f64::min)
    } else {
        evaluated
            .iter()
            .map(|v| v.number())
            .fold(f64::NEG_INFINITY, f64::max)
    };
    combine(first, pick)
}

/// Rebuild a value of the same kind as `like` from a plain number.
fn combine(like: Value, number: f64) -> Option<Value> {
    Some(match like {
        Value::Px(_) => Value::Px(number),
        Value::Number(_) => Value::Number(number),
    })
}

/// A position in a run of component values, skipping whitespace on
/// demand — whitespace is syntax in CSS math, since `a - b` and `a -b`
/// differ, but it is not something the grammar above needs to see.
struct Cursor<'a> {
    values: &'a [ComponentValue],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(values: &'a [ComponentValue]) -> Self {
        Self { values, pos: 0 }
    }

    fn skip_whitespace(&mut self) {
        while matches!(
            self.values.get(self.pos),
            Some(ComponentValue::Token(CssToken::Whitespace))
        ) {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<&'a ComponentValue> {
        self.skip_whitespace();
        self.values.get(self.pos)
    }

    /// The operator here, if it is one of `wanted`.
    fn operator(&mut self, wanted: &[char]) -> Option<char> {
        match self.peek() {
            Some(ComponentValue::Token(CssToken::Delim(symbol))) if wanted.contains(symbol) => {
                self.pos += 1;
                Some(*symbol)
            }
            _ => None,
        }
    }

    fn advance(&mut self) -> Option<&'a ComponentValue> {
        let value = self.peek()?;
        self.pos += 1;
        Some(value)
    }
}

fn sum(cursor: &mut Cursor, context: &LengthContext, depth: u8) -> Option<Value> {
    let mut total = product(cursor, context, depth)?;
    while let Some(symbol) = cursor.operator(&['+', '-']) {
        let right = product(cursor, context, depth)?;
        // Adding a length to a bare number is the mismatch the spec
        // rejects: `calc(10px + 2)` means nothing.
        total = match (total, right) {
            (Value::Px(a), Value::Px(b)) => Value::Px(if symbol == '+' { a + b } else { a - b }),
            (Value::Number(a), Value::Number(b)) => {
                Value::Number(if symbol == '+' { a + b } else { a - b })
            }
            _ => return None,
        };
    }
    Some(total)
}

fn product(cursor: &mut Cursor, context: &LengthContext, depth: u8) -> Option<Value> {
    let mut total = value(cursor, context, depth)?;
    while let Some(symbol) = cursor.operator(&['*', '/']) {
        let right = value(cursor, context, depth)?;
        total = match symbol {
            // Multiplying two lengths gives an area, which CSS has no
            // way to express — one side has to be a plain number.
            '*' => match (total, right) {
                (Value::Px(a), Value::Number(b)) | (Value::Number(b), Value::Px(a)) => {
                    Value::Px(a * b)
                }
                (Value::Number(a), Value::Number(b)) => Value::Number(a * b),
                (Value::Px(_), Value::Px(_)) => return None,
            },
            // Only a plain number may divide, and never by zero.
            _ => match right {
                Value::Number(divisor) if divisor != 0.0 => match total {
                    Value::Px(a) => Value::Px(a / divisor),
                    Value::Number(a) => Value::Number(a / divisor),
                },
                _ => return None,
            },
        };
    }
    Some(total)
}

fn value(cursor: &mut Cursor, context: &LengthContext, depth: u8) -> Option<Value> {
    match cursor.advance()? {
        ComponentValue::Token(CssToken::Number { value, .. }) => Some(Value::Number(*value)),
        ComponentValue::Token(CssToken::Dimension { value, unit }) => {
            let unit = Unit::parse(unit)?;
            Some(Value::Px(
                Length {
                    value: *value as f32,
                    unit,
                }
                .to_px(context) as f64,
            ))
        }
        // A percentage has no length to be a percentage of here; see
        // the module note on why it is refused rather than guessed.
        ComponentValue::Token(CssToken::Percentage(_)) => None,
        // `calc(2 * (1 + 1))`: parentheses group, nothing more.
        ComponentValue::Block { kind, values } if *kind == super::parser::BlockKind::Paren => {
            sum(&mut Cursor::new(values), context, depth + 1)
        }
        ComponentValue::Function { name, arguments } if is_math(name) => {
            call(name, arguments, context, depth + 1)
        }
        _ => None,
    }
}

/// Split on top-level commas: the argument separator.
fn split_on_commas(values: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    let mut out = vec![Vec::new()];
    for value in values {
        if matches!(value, ComponentValue::Token(CssToken::Comma)) {
            out.push(Vec::new());
        } else {
            out.last_mut()
                .expect("one part is always open")
                .push(value.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::{parse, serialize, Rule};

    /// Evaluate `property`'s value in a 16px font, 1000x800 viewport.
    fn eval(css: &str) -> String {
        let sheet = parse(&format!("a {{ x: {css} }}"));
        let Some(Rule::Style(style)) = sheet.rules.first() else {
            panic!("not a style rule");
        };
        let context = LengthContext {
            font_size: 16.0,
            root_font_size: 16.0,
            viewport_width: 1000.0,
            viewport_height: 800.0,
        };
        serialize(&evaluate_all(&style.declarations[0].value, &context))
    }

    #[test]
    fn calc_adds_and_subtracts_lengths() {
        assert_eq!(eval("calc(10px + 5px)"), "15px");
        assert_eq!(eval("calc(10px - 5px)"), "5px");
    }

    #[test]
    fn calc_multiplies_a_length_by_a_number() {
        assert_eq!(eval("calc(10px * 3)"), "30px");
        assert_eq!(eval("calc(3 * 10px)"), "30px");
        assert_eq!(eval("calc(30px / 3)"), "10px");
    }

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        assert_eq!(eval("calc(2px + 3px * 4)"), "14px");
        assert_eq!(eval("calc((2px + 3px) * 4)"), "20px");
    }

    #[test]
    fn relative_units_resolve_against_the_context() {
        assert_eq!(eval("calc(2rem)"), "32px");
        assert_eq!(eval("calc(10vw)"), "100px");
        assert_eq!(eval("calc(1em + 4px)"), "20px");
    }

    #[test]
    fn clamp_picks_the_middle_value_within_its_bounds() {
        // The pattern every responsive heading is written with.
        assert_eq!(eval("clamp(32px, 4.2vw, 48px)"), "42px");
        assert_eq!(eval("clamp(32px, 1vw, 48px)"), "32px", "below the floor");
        assert_eq!(eval("clamp(32px, 90vw, 48px)"), "48px", "above the ceiling");
    }

    #[test]
    fn min_and_max_take_any_number_of_arguments() {
        assert_eq!(eval("min(10px, 4px, 7px)"), "4px");
        assert_eq!(eval("max(10px, 4px, 7px)"), "10px");
    }

    #[test]
    fn a_bare_number_stays_unitless() {
        // `line-height: calc(1.2 * 1)` must not become 1.2px.
        assert_eq!(eval("calc(1.2 * 1)"), "1.2");
        assert_eq!(eval("calc(4 / 2)"), "2");
    }

    #[test]
    fn math_functions_nest() {
        assert_eq!(eval("calc(10px + max(2px, 5px))"), "15px");
        assert_eq!(eval("max(1px, calc(2px * 3))"), "6px");
    }

    #[test]
    fn a_percentage_is_left_for_layout_to_answer() {
        // Untouched, so the property parser rejects it rather than the
        // engine inventing a basis.
        assert_eq!(eval("calc(100% - 20px)"), "calc(100% - 20px)");
    }

    #[test]
    fn a_unit_mismatch_evaluates_to_nothing() {
        assert_eq!(eval("calc(10px + 2)"), "calc(10px + 2)");
        assert_eq!(eval("calc(10px * 2px)"), "calc(10px * 2px)");
        assert_eq!(eval("calc(10px / 2px)"), "calc(10px / 2px)");
    }

    #[test]
    fn division_by_zero_is_refused() {
        assert_eq!(eval("calc(10px / 0)"), "calc(10px / 0)");
    }

    #[test]
    fn a_math_function_inside_another_function_is_evaluated() {
        assert_eq!(eval("rgb(0 0 0 / calc(1 / 2))"), "rgb(0 0 0 / 0.5)");
    }

    #[test]
    fn a_value_with_no_math_in_it_is_unchanged() {
        assert_eq!(eval("0 auto"), "0 auto");
        assert_eq!(eval("1px solid red"), "1px solid red");
    }
}
