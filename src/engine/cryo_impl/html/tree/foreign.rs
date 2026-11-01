//! Putting SVG and MathML names back the way they were written.
//!
//! The HTML tokenizer lowercases every attribute name it reads,
//! because HTML's own are case-insensitive. SVG's are not: `viewBox`
//! is a different name from `viewbox`, and an SVG renderer looking for
//! the first finds nothing.
//!
//! So the HTML standard carries a table — *adjust SVG attributes* —
//! that maps each lowercased form back. This is that table.
//!
//! It is applied to **every** element, not only to ones inside an
//! `<svg>`. That looks careless and is not: no HTML element has an
//! attribute named `viewbox`, `refx` or `stddeviation`, so there is
//! nothing for the table to damage, and applying it unconditionally
//! avoids threading "am I in foreign content" through element
//! creation for no gain.
//!
//! Without this, an inline `<svg viewBox="0 0 24 24">` — which is how
//! every icon on a modern page is written — loses the one attribute
//! that says how big its drawing is, and renders at 24 pixels
//! whatever size it was given.

/// The lowercased name, and the name as SVG spells it.
///
/// From the HTML Standard's *adjust SVG attributes* table.
const ADJUSTED: [(&str, &str); 58] = [
    ("attributename", "attributeName"),
    ("attributetype", "attributeType"),
    ("basefrequency", "baseFrequency"),
    ("baseprofile", "baseProfile"),
    ("calcmode", "calcMode"),
    ("clippathunits", "clipPathUnits"),
    ("diffuseconstant", "diffuseConstant"),
    ("edgemode", "edgeMode"),
    ("filterunits", "filterUnits"),
    ("glyphref", "glyphRef"),
    ("gradienttransform", "gradientTransform"),
    ("gradientunits", "gradientUnits"),
    ("kernelmatrix", "kernelMatrix"),
    ("kernelunitlength", "kernelUnitLength"),
    ("keypoints", "keyPoints"),
    ("keysplines", "keySplines"),
    ("keytimes", "keyTimes"),
    ("lengthadjust", "lengthAdjust"),
    ("limitingconeangle", "limitingConeAngle"),
    ("markerheight", "markerHeight"),
    ("markerunits", "markerUnits"),
    ("markerwidth", "markerWidth"),
    ("maskcontentunits", "maskContentUnits"),
    ("maskunits", "maskUnits"),
    ("numoctaves", "numOctaves"),
    ("pathlength", "pathLength"),
    ("patterncontentunits", "patternContentUnits"),
    ("patterntransform", "patternTransform"),
    ("patternunits", "patternUnits"),
    ("pointsatx", "pointsAtX"),
    ("pointsaty", "pointsAtY"),
    ("pointsatz", "pointsAtZ"),
    ("preservealpha", "preserveAlpha"),
    ("preserveaspectratio", "preserveAspectRatio"),
    ("primitiveunits", "primitiveUnits"),
    ("refx", "refX"),
    ("refy", "refY"),
    ("repeatcount", "repeatCount"),
    ("repeatdur", "repeatDur"),
    ("requiredextensions", "requiredExtensions"),
    ("requiredfeatures", "requiredFeatures"),
    ("specularconstant", "specularConstant"),
    ("specularexponent", "specularExponent"),
    ("spreadmethod", "spreadMethod"),
    ("startoffset", "startOffset"),
    ("stddeviation", "stdDeviation"),
    ("stitchtiles", "stitchTiles"),
    ("surfacescale", "surfaceScale"),
    ("systemlanguage", "systemLanguage"),
    ("tablevalues", "tableValues"),
    ("targetx", "targetX"),
    ("targety", "targetY"),
    ("textlength", "textLength"),
    ("viewbox", "viewBox"),
    ("viewtarget", "viewTarget"),
    ("xchannelselector", "xChannelSelector"),
    ("ychannelselector", "yChannelSelector"),
    ("zoomandpan", "zoomAndPan"),
];

/// The name as SVG spells it, or the name unchanged.
pub(super) fn adjust_attribute(name: &str) -> &str {
    ADJUSTED
        .iter()
        .find(|(lowered, _)| *lowered == name)
        .map_or(name, |(_, spelled)| *spelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_box_gets_its_capital_back() {
        // The one that matters: every inline icon carries it.
        assert_eq!(adjust_attribute("viewbox"), "viewBox");
    }

    #[test]
    fn an_html_attribute_is_left_alone() {
        assert_eq!(adjust_attribute("class"), "class");
        assert_eq!(adjust_attribute("href"), "href");
        assert_eq!(adjust_attribute("width"), "width");
    }

    #[test]
    fn the_filter_attributes_are_covered_too() {
        assert_eq!(adjust_attribute("stddeviation"), "stdDeviation");
        assert_eq!(
            adjust_attribute("preserveaspectratio"),
            "preserveAspectRatio"
        );
    }

    #[test]
    fn every_entry_maps_a_lowercased_form_to_itself() {
        // A typo in the table would otherwise be a name that never
        // matches anything.
        for (lowered, spelled) in ADJUSTED {
            assert_eq!(
                spelled.to_ascii_lowercase(),
                lowered,
                "{spelled} does not lowercase to {lowered}"
            );
        }
    }

    #[test]
    fn no_entry_collides_with_a_plain_html_attribute() {
        // Why the table may be applied unconditionally — see the
        // module note.
        const HTML: [&str; 12] = [
            "class", "id", "style", "src", "href", "alt", "title", "width", "height", "type",
            "name", "value",
        ];
        for (lowered, _) in ADJUSTED {
            assert!(!HTML.contains(&lowered), "{lowered} is an HTML attribute");
        }
    }
}
