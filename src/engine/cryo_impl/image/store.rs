//! Where the engine keeps the images a page has asked for.
//!
//! Paint needs pixels, but the display list carries URLs: it is built
//! long before anything has been fetched, and the same image is often
//! drawn in several places. So the two meet here — a map from the URL
//! a stylesheet wrote to the picture that arrived.
//!
//! The lookup is a *port*: paint declares what it needs ([`Images`])
//! and does not know whether the answer came off a disk, a network or
//! a test's fixture. [`Store`] is the ordinary adapter, and a test can
//! hand paint anything else that answers the same question.

use std::collections::BTreeMap;

use super::Image;

/// What paint needs in order to draw a picture.
pub trait Images {
    /// The image at this URL, or `None` if it never arrived.
    ///
    /// A missing image is not an error: the page still renders, with
    /// a hole where the picture would be. That is what a browser does
    /// while one is loading and what it keeps doing if it never
    /// comes.
    fn get(&self, url: &str) -> Option<&Image>;
}

/// The images a page has fetched, by resolved URL.
#[derive(Debug, Default)]
pub struct Store {
    images: BTreeMap<String, Image>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, url: impl Into<String>, image: Image) {
        self.images.insert(url.into(), image);
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

impl Images for Store {
    fn get(&self, url: &str) -> Option<&Image> {
        self.images.get(url)
    }
}

/// A source with nothing in it, for rendering without images.
pub struct NoImages;

impl Images for NoImages {
    fn get(&self, _url: &str) -> Option<&Image> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel() -> Image {
        Image::new(1, 1, vec![1, 2, 3, 255]).expect("one pixel")
    }

    #[test]
    fn an_image_comes_back_by_the_url_it_went_in_under() {
        let mut store = Store::new();
        store.insert("https://a.test/x.png", pixel());
        assert!(store.get("https://a.test/x.png").is_some());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_url_that_never_arrived_is_none_rather_than_an_error() {
        let store = Store::new();
        assert!(store.get("https://a.test/missing.png").is_none());
        assert!(store.is_empty());
    }

    #[test]
    fn the_empty_source_answers_nothing() {
        assert!(NoImages.get("https://a.test/x.png").is_none());
    }
}
