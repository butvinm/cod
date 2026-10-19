//! Where Cod opens links (issue #32): http and https links become page tabs, everything else goes to the system as in Zed.
//!
//! The workspace crate cannot depend on the page tabs of `web_view`, so `web_view::init` installs a `CodPageOpener`. Without one (other platforms, the old web view backend, tests), every link goes to the system.

use std::rc::Rc;

use gpui::{App, Context, Global, Window};
use url::Url;

use crate::Workspace;

type OpenPage = dyn Fn(&mut Workspace, String, &mut Window, &mut Context<Workspace>);

/// Opens an http or https URL as a page tab of the workspace: it focuses the workspace's tab that already shows the URL, or adds a new tab to the active pane.
#[derive(Clone)]
pub struct CodPageOpener(Rc<OpenPage>);

impl Global for CodPageOpener {}

impl CodPageOpener {
    pub fn install(
        open: impl Fn(&mut Workspace, String, &mut Window, &mut Context<Workspace>) + 'static,
        cx: &mut App,
    ) {
        cx.set_global(Self(Rc::new(open)));
    }
}

/// Whether a link opens in a page tab: only absolute http and https URLs do.
pub fn opens_in_page(link: &str) -> bool {
    Url::parse(link).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

impl Workspace {
    /// Opens a link from a chat, a file or a preview: http and https in a page tab, other schemes (mailto and the like) through the system.
    pub fn cod_open_link(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        if opens_in_page(link) {
            if let Some(opener) = cx.try_global::<CodPageOpener>().cloned() {
                (opener.0)(self, link.to_owned(), window, cx);
                return;
            }
        }
        cx.open_url(link);
    }
}

#[cfg(test)]
mod tests {
    use super::opens_in_page;

    #[test]
    fn only_web_links_open_in_a_page() {
        assert!(opens_in_page("http://127.0.0.1:8000/tracker/PAY-1234"));
        assert!(opens_in_page("https://example.com/wiki/Home"));
        assert!(!opens_in_page("mailto:dev@example.com"));
        assert!(!opens_in_page("file:///tmp/notes.md"));
        assert!(!opens_in_page("zed://settings"));
        assert!(!opens_in_page("example.com"));
    }
}
