//! Pictures from the web: avatars, and images in issues and comments. Each
//! URL is fetched once, on a thread of its own, and decoded by Rune's image
//! cache, which wakes the app when it's ready.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rune::{Image, ImageCache, Waker};

use crate::App;

/// Larger responses aren't images zbugs shows.
const MAX_BYTES: u64 = 20 << 20;

#[derive(Clone)]
pub struct Images(Rc<RefCell<State>>);

struct State {
    ui: rune::AppHandle<App>,
    cache: Option<ImageCache>,
    /// `None` while fetching, or when the fetch failed.
    images: HashMap<String, Option<Image>>,
    /// Those still decoding: the view shows them once they're done.
    decoding: Vec<Image>,
}

impl Images {
    pub fn new(ui: rune::AppHandle<App>) -> Images {
        Images(Rc::new(RefCell::new(State {
            ui,
            cache: None,
            images: HashMap::new(),
            decoding: Vec::new(),
        })))
    }

    /// Rune's waker, from `AppSpec::setup`: there are no pictures before it.
    pub fn start(&self, waker: Waker) {
        self.0.borrow_mut().cache = Some(ImageCache::new(waker));
    }

    /// The picture at `url`, once it has arrived; the first ask fetches it.
    pub fn get(&self, url: &str) -> Option<Image> {
        let mut state = self.0.borrow_mut();
        if let Some(image) = state.images.get(url) {
            return image.clone();
        }
        if state.cache.is_none() || !(url.starts_with("https://") || url.starts_with("http://")) {
            return None;
        }
        state.images.insert(url.to_owned(), None);
        let (ui, url) = (state.ui.clone(), url.to_owned());
        std::thread::spawn(move || {
            let bytes = fetch(&url);
            ui.update(move |s, _| {
                if let Some(bytes) = bytes {
                    s.images.arrived(url, bytes);
                }
            });
        });
        None
    }

    fn arrived(&self, url: String, bytes: Vec<u8>) {
        let mut state = self.0.borrow_mut();
        let image = state.cache.as_ref().map(|c| c.load(bytes));
        state.decoding.extend(image.clone());
        state.images.insert(url, image);
    }

    /// From `AppSpec::poll`, which the cache's waker runs: whether a picture
    /// finished decoding, so the view shows it. Markdown reads a picture
    /// only as it builds, and nothing else changes then.
    pub fn poll(&self) -> bool {
        let mut state = self.0.borrow_mut();
        let before = state.decoding.len();
        state.decoding.retain(|i| !i.is_ready());
        state.decoding.len() != before
    }
}

fn fetch(url: &str) -> Option<Vec<u8>> {
    let mut response = ureq::get(url).call().ok()?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .ok()
}

/// A GitHub avatar at the size zbugs shows it, as the web app asks for.
pub fn avatar_url(url: &str) -> String {
    if url.starts_with("https://avatars.githubusercontent.com/") && !url.contains("size=") {
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}size=64")
    } else {
        url.to_owned()
    }
}
