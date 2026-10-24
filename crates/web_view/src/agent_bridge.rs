//! The small Cod action bridge for agent-made pages (#43), built on #31/#32 pages and #37/#38
//! `codctl`.
//!
//! An agent page is built once, in Rust, from the HTML `codctl open --html` was given: the page's
//! own inline scripts are stripped unless `--js` was given, and Cod's own bootstrap script is
//! always appended, regardless. The bootstrap defines `window.cod.call(name, args)` and wires
//! every `[data-cod]` element to it; a call becomes one `console.log` line behind a sentinel
//! prefix, which Cod's side (`offscreen.rs`) reads apart from the page's own console output and
//! checks against an allowlist here, in Cod, never in the page. The result is served as a `data:`
//! URL, so the page never gets file-system access of its own and a navigation away from it loses
//! the bridge (it only ever existed in that one document).

use gpui::{App, Global};
use serde_json::Value;

use crate::browser_client::protocol;
use crate::pages::PageId;

/// Every line the page's bootstrap script prints on Cod's behalf starts with this, so Cod can
/// tell a bridge call from the page's own console output. No real page is expected to start a log
/// line this way. Defined once, in `protocol.rs` (RC review B5), so the engine process (which has
/// no notion of `agent_bridge` at all) can give a bridge call its own, larger byte limit.
pub const BRIDGE_PREFIX: &str = protocol::BRIDGE_PREFIX;

/// Every action an agent page may ask for; anything else is refused and logged (#43 AC3, AC4).
pub const ALLOWED_ACTIONS: &[&str] = &["submit", "open-file", "focus-session"];

/// One call from a page's bootstrap script.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeCall {
    pub name: String,
    pub args: Value,
    /// The document token the call carries (`document_token`), if any.
    pub token: Option<String>,
}

/// Reads a bridge call from one console line, if it is one.
pub fn parse_bridge_message(line: &str) -> Option<BridgeCall> {
    let payload = line.strip_prefix(BRIDGE_PREFIX)?;
    let value: Value = serde_json::from_str(payload).ok()?;
    let name = value.get("name")?.as_str()?.to_owned();
    let args = value.get("args").cloned().unwrap_or(Value::Null);
    let token = value.get("t").and_then(Value::as_str).map(str::to_owned);
    Some(BridgeCall { name, args, token })
}

/// The marker in Cod's bootstrap script just before the document's token.
const TOKEN_MARKER: &str = ",t:'";

/// The random token Cod's own bootstrap script puts in every bridge call of this agent document,
/// read back from the document's `data:` URL (RC review residual, B2 for `--js` pages). Cod's
/// prelude always comes first in the document, so the first `<script>` element is Cod's
/// bootstrap, never page content. The token lives only in that script's closure: a subframe of
/// another origin cannot read it, so a console line it forges carries no valid token, whatever
/// source CEF reports for that line.
pub fn document_token(document_url: &str) -> Option<String> {
    use base64::Engine as _;
    let encoded = document_url.strip_prefix("data:text/html;charset=utf-8;base64,")?;
    let html = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let html = String::from_utf8(html).ok()?;
    let bootstrap = &html[..html.find("</script>")?];
    let start = bootstrap.find(TOKEN_MARKER)? + TOKEN_MARKER.len();
    let length = bootstrap[start..].find('\'')?;
    Some(bootstrap[start..start + length].to_owned())
}

/// A string argument of a bridge call, or empty when it is missing.
pub fn arg_str(call: &BridgeCall, key: &str) -> String {
    call.args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Whether `name` is in the allowlist Cod checks before acting on a bridge call (#43 AC3, AC4,
/// security requirement #7: the allowlist is checked here, not in the page).
pub fn is_allowed(name: &str) -> bool {
    ALLOWED_ACTIONS.contains(&name)
}

/// Splits `path:line:column` or `path:line` the way `codctl open` does, so a page's
/// `data-cod-path` can point at a line like `src/invoice.rs:18`.
pub fn split_path_position(target: &str) -> (&str, Option<u32>, Option<u32>) {
    let number = |text: &str| {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse::<u32>().ok().filter(|number| *number > 0)
    };
    if let Some((rest, last)) = target.rsplit_once(':')
        && let Some(last) = number(last)
    {
        if let Some((path, middle)) = rest.rsplit_once(':')
            && let Some(middle) = number(middle)
            && !path.is_empty()
        {
            return (path, Some(middle), Some(last));
        }
        if !rest.is_empty() {
            return (rest, Some(last), None);
        }
    }
    (target, None, None)
}

/// The current theme's colors and font, injected as CSS custom properties (#43 AC6).
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeVars {
    pub background: String,
    pub text: String,
    pub accent: String,
    pub font: String,
}

impl ThemeVars {
    /// The `document.documentElement.style.setProperty` calls that apply these variables; used
    /// both in the page's bootstrap script and to update a live page after a theme switch,
    /// without a reload.
    fn set_script(&self) -> String {
        format!(
            "document.documentElement.style.setProperty('--cod-background',{background});\
             document.documentElement.style.setProperty('--cod-text',{text});\
             document.documentElement.style.setProperty('--cod-accent',{accent});\
             document.documentElement.style.setProperty('--cod-font',{font});",
            background = js_string(&self.background),
            text = js_string(&self.text),
            accent = js_string(&self.accent),
            font = js_string(&self.font),
        )
    }
}

/// Reads the colors and font the running Cod shows now, as `ThemeVars`.
pub fn current_theme_vars(cx: &App) -> ThemeVars {
    use settings::Settings as _;
    use theme::ActiveTheme as _;

    let colors = cx.theme().colors();
    let css = |color: gpui::Hsla| {
        let rgba = color.to_rgb();
        format!(
            "rgba({}, {}, {}, {:.3})",
            (rgba.r * 255.0).round() as u8,
            (rgba.g * 255.0).round() as u8,
            (rgba.b * 255.0).round() as u8,
            rgba.a,
        )
    };
    let font = theme_settings::ThemeSettings::get_global(cx)
        .buffer_font
        .family
        .to_string();
    ThemeVars {
        background: css(colors.background),
        text: css(colors.text),
        accent: css(colors.text_accent),
        font,
    }
}

/// A JavaScript single-quoted string literal of `text`, escaping backslashes, quotes and newlines.
fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

/// Cod's bootstrap script for an agent page (#43 AC3, AC4, AC6): theme variables, `window.cod.call`
/// and the `[data-cod]` click wiring. It always runs, even when the page's own inline scripts do
/// not: it is Cod's own code, appended after they are stripped, never the page's.
///
/// `nonce`, when given, is Cod's own Content-Security-Policy nonce (#43 security requirement,
/// RC review B1): it names this `<script>` element as the one the page's CSP allows to run, so
/// the bootstrap keeps working even while every other script and inline event handler the page's
/// own content tries to run is blocked.
///
/// `token` is the document's own bridge token (`document_token`): every call carries it, and Cod
/// acts only on calls with the token of the agent document it built.
pub fn bootstrap_script(vars: &ThemeVars, nonce: Option<&str>, token: &str) -> String {
    let nonce_attr = match nonce {
        Some(nonce) => format!(" nonce=\"{nonce}\""),
        None => String::new(),
    };
    format!(
        "<script{nonce_attr}>(function(){{\
         {theme_vars}\
         function codSend(name,args){{console.log({prefix}+JSON.stringify({{name:name,args:args||{{}}{TOKEN_MARKER}{token}'}}));}}\
         window.cod={{call:codSend}};\
         document.addEventListener('click',function(e){{\
         var el=e.target&&e.target.closest?e.target.closest('[data-cod]'):null;\
         if(!el)return;\
         var name=el.getAttribute('data-cod');\
         var args={{}};\
         if(name==='submit'){{\
         var sel=el.getAttribute('data-cod-value-from');\
         var field=sel?document.querySelector(sel):null;\
         args.value=field?field.value:'';\
         }}else if(name==='open-file'){{\
         args.path=el.getAttribute('data-cod-path')||'';\
         }}else if(name==='focus-session'){{\
         args.session=el.getAttribute('data-cod-session')||el.textContent||'';\
         }}\
         e.preventDefault();\
         codSend(name,args);\
         }},true);\
         }})();</script>",
        theme_vars = vars.set_script(),
        prefix = js_string(BRIDGE_PREFIX),
    )
}

/// A fresh, unpredictable Content-Security-Policy nonce (#43 security requirement, RC review
/// B1): generated after the page's own HTML has already been read, so nothing in that HTML can
/// ever carry a matching value, however it was built.
fn new_nonce() -> String {
    use base64::Engine as _;
    use rand::RngCore as _;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The `<meta>` Content-Security-Policy that keeps every script but Cod's own nonce-tagged
/// bootstrap from running (#43 security requirement, RC review B1): this blocks `<script>`
/// elements the page's own content still carries as well as inline event handler attributes
/// (`onerror`, `onclick`, ...), which stripping `<script>` tags alone never touched.
///
/// `frame-src`, `child-src` and `object-src` are all `'none'` (RC review B2): without `--js`, a
/// page cannot embed any subframe at all, so an `<iframe>` with a `srcdoc` or `data:` document of
/// its own can never run console script that forges a `cod:bridge:` message. Console messages
/// carry no frame identity today, so refusing every subframe is the only thing that stops one
/// from impersonating the top document while this policy applies.
fn csp_meta_tag(nonce: &str) -> String {
    format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'nonce-{nonce}'; frame-src 'none'; child-src 'none'; object-src 'none'\">"
    )
}

/// The script that updates an already-open page's theme variables without a reload (#43 AC6).
pub fn theme_update_script(vars: &ThemeVars) -> String {
    vars.set_script()
}

/// Removes every `<script>...</script>` block from `html`, case-insensitively, leaving everything
/// else as it was (#43 AC4: inline scripts do not run unless `--js` was given).
pub fn strip_inline_scripts(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut lower_rest = lower.as_str();
    loop {
        let Some(start) = lower_rest.find("<script") else {
            out.push_str(rest);
            break;
        };
        // A tag name can continue past "<script" (for example "<scripture>"); only a real
        // script element starts here, where the name ends at whitespace, '>' or '/'.
        let after_name = lower_rest[start + "<script".len()..].chars().next();
        if !after_name.is_some_and(|ch| ch.is_whitespace() || ch == '>' || ch == '/') {
            let keep_to = start + "<script".len();
            out.push_str(&rest[..keep_to]);
            rest = &rest[keep_to..];
            lower_rest = &lower_rest[keep_to..];
            continue;
        }
        out.push_str(&rest[..start]);
        let after_start = match lower_rest[start..].find('>') {
            Some(offset) => start + offset + 1,
            None => {
                // An unterminated opening tag: there is no script content left to keep.
                break;
            }
        };
        let end = lower_rest[after_start..]
            .find("</script>")
            .map(|offset| after_start + offset + "</script>".len())
            .unwrap_or(lower_rest.len());
        rest = &rest[end..];
        lower_rest = &lower_rest[end..];
    }
    out
}

/// Cod's whole processing of an agent page's source (#43 AC1, AC4, AC6): strips the page's own
/// inline scripts unless `allow_js`, then always emits Cod's own prelude first, followed by the
/// page's original markup unchanged (RC review B1).
///
/// Earlier builds looked for the page's own `<head>`, `<html>` or `<!doctype>` to splice into,
/// which a source document could defeat just by hiding its `<head>` inside an HTML comment
/// (`<!-- <head> -->`): the splice landed inside the comment, so neither the CSP nor the
/// bootstrap ever took effect. Instead, Cod now always writes a complete, fixed document of its
/// own - `<!doctype html><html><head>{csp}{bootstrap}</head>` - and appends the page's original
/// markup after it, untouched. A real HTML parser treats a second `<!doctype>`, `<html>` or
/// `<head>` start tag appearing later as a parse error and ignores it, while tags that belong in
/// `<head>` (`<meta>`, `<title>`, `<link>`, `<style>`) are still moved there even when they show
/// up in the body, so the page's own head content still ends up in the right place; only Cod's
/// own prelude ever decides where the CSP and the bootstrap go.
///
/// Without `allow_js`, a Content-Security-Policy that allows only Cod's own, freshly nonced
/// bootstrap script is emitted alongside it (RC review B1): stripping `<script>` elements alone
/// leaves inline event handlers (`onerror="..."`, `onclick="..."`, ...) free to run and forge a
/// bridge call, which the CSP blocks at the engine instead. `allow_js` is explicit, informed
/// consent to run the page's own JavaScript, so no CSP is added for it: the page's scripts run as
/// they always have, Cod's bootstrap included.
pub fn build_agent_document(source_html: &str, allow_js: bool, vars: &ThemeVars) -> String {
    let token = new_nonce();
    if allow_js {
        return format!(
            "<!doctype html><html><head>{}</head>{source_html}",
            bootstrap_script(vars, None, &token)
        );
    }
    let body = strip_inline_scripts(source_html);
    let nonce = new_nonce();
    format!(
        "<!doctype html><html><head>{}{}</head>{body}",
        csp_meta_tag(&nonce),
        bootstrap_script(vars, Some(&nonce), &token)
    )
}

/// The current form of an agent document saved before bridge tokens existed, or None when
/// `document_url` already has a token or is not an agent document Cod built with its prelude
/// first. Such a document's bootstrap sends calls without a token, which Cod now refuses, so on
/// restore it is rebuilt from the page markup after Cod's old prelude: with a fresh token, and
/// with the CSP exactly when the old prelude had one (the page was opened without `--js`; its
/// markup was already stripped of scripts then).
pub fn upgrade_legacy_document(document_url: &str, vars: &ThemeVars) -> Option<String> {
    use base64::Engine as _;
    if document_token(document_url).is_some() {
        return None;
    }
    let encoded = document_url.strip_prefix("data:text/html;charset=utf-8;base64,")?;
    let html = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let html = String::from_utf8(html).ok()?;
    let rest = html.strip_prefix("<!doctype html><html><head>")?;
    let prelude_end = rest.find("</head>")?;
    let prelude = &rest[..prelude_end];
    if !prelude.contains("window.cod={call:codSend}") {
        return None;
    }
    let markup = &rest[prelude_end + "</head>".len()..];
    let allow_js = !prelude.contains("Content-Security-Policy");
    Some(data_url(&build_agent_document(markup, allow_js, vars)))
}

/// A `data:` URL that loads `html` as a document (#43 AC1, AC5, open question 1): used instead of
/// `file://` so an agent page never gets direct file-system access of its own, and the bridge
/// cannot be carried to a page opened any other way.
pub fn data_url(html: &str) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(html.as_bytes());
    format!("data:text/html;charset=utf-8;base64,{encoded}")
}

/// What `codctl open --html --wait` gets when the page tab ends (#43 AC2).
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitOutcome {
    /// The user submitted this value; the page tab is already closed.
    Submitted(String),
    /// The user closed the tab instead of submitting.
    Closed,
}

/// One `codctl open --html --wait` waiting for its page, and the value the page submitted, if any.
struct Waiter {
    sender: futures::channel::oneshot::Sender<SubmitOutcome>,
    submitted: Option<String>,
}

/// The pages `codctl open --html --wait` waits for (#43 AC2). A submit only records its value: the waiter is answered once the page tab is gone, so `codctl` exits after the tab closed, as AC2 orders it, and the next command never still sees the submitted page.
#[derive(Default)]
struct Waiters(std::collections::BTreeMap<PageId, Waiter>);

impl Global for Waiters {}

impl Waiters {
    fn register(&mut self, page: PageId) -> futures::channel::oneshot::Receiver<SubmitOutcome> {
        let (sender, receiver) = futures::channel::oneshot::channel();
        self.0.insert(
            page,
            Waiter {
                sender,
                submitted: None,
            },
        );
        receiver
    }

    fn record_submit(&mut self, page: &PageId, value: String) {
        if let Some(waiter) = self.0.get_mut(page) {
            waiter.submitted = Some(value);
        }
    }

    fn finish(&mut self, page: &PageId) {
        if let Some(waiter) = self.0.remove(page) {
            let outcome = match waiter.submitted {
                Some(value) => SubmitOutcome::Submitted(value),
                None => SubmitOutcome::Closed,
            };
            waiter.sender.send(outcome).ok();
        }
    }
}

/// Registers a waiter for the page's submit or close, for `codctl open --html --wait` (#43 AC2).
pub fn wait_for_submit(page: PageId, cx: &mut App) -> futures::channel::oneshot::Receiver<SubmitOutcome> {
    cx.default_global::<Waiters>().register(page)
}

/// Records that the user submitted `value` on the page; its waiter gets it once the page tab is gone (`closed`). A no-op without a waiter.
pub fn submit(page: &PageId, value: String, cx: &mut App) {
    cx.default_global::<Waiters>().record_submit(page, value);
}

/// Answers the page's waiter now that its tab is gone: the submitted value, or `Closed` when the user closed the tab without submitting. A no-op without a waiter.
pub fn closed(page: &PageId, cx: &mut App) {
    cx.default_global::<Waiters>().finish(page);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> ThemeVars {
        ThemeVars {
            background: "rgba(10, 10, 10, 1.000)".into(),
            text: "rgba(255, 255, 255, 1.000)".into(),
            accent: "rgba(80, 120, 250, 1.000)".into(),
            font: "Zed Sans".into(),
        }
    }

    #[test]
    fn bridge_messages_are_told_apart_from_normal_console_output() {
        assert!(parse_bridge_message("hello").is_none());
        assert!(parse_bridge_message("cod:bridge:not json").is_none());
        let call =
            parse_bridge_message(r#"cod:bridge:{"name":"submit","args":{"value":"half-even"}}"#)
                .unwrap();
        assert_eq!(call.name, "submit");
        assert_eq!(arg_str(&call, "value"), "half-even");
    }

    #[test]
    fn only_the_allowed_actions_pass() {
        assert!(is_allowed("submit"));
        assert!(is_allowed("open-file"));
        assert!(is_allowed("focus-session"));
        assert!(!is_allowed("run-shell"));
        assert!(!is_allowed(""));
    }

    #[test]
    fn file_positions_split_like_codctl_open() {
        assert_eq!(split_path_position("src/invoice.rs:18:9"), ("src/invoice.rs", Some(18), Some(9)));
        assert_eq!(split_path_position("src/invoice.rs:18"), ("src/invoice.rs", Some(18), None));
        assert_eq!(split_path_position("src/invoice.rs"), ("src/invoice.rs", None, None));
    }

    #[test]
    fn inline_scripts_are_stripped_but_other_tags_survive() {
        let html = "<body><h1 id=\"h\">Ask</h1><script>document.getElementById('h').textContent='changed';</script><p>end</p></body>";
        let stripped = strip_inline_scripts(html);
        assert!(!stripped.contains("<script"));
        assert!(stripped.contains("<h1 id=\"h\">Ask</h1>"));
        assert!(stripped.contains("<p>end</p>"));
    }

    #[test]
    fn a_tag_that_only_starts_with_script_is_left_alone() {
        let html = "<scripture>not a script</scripture>";
        assert_eq!(strip_inline_scripts(html), html);
    }

    #[test]
    fn the_bootstrap_script_starts_the_head() {
        let html = "<!doctype html><html><head><meta charset=\"utf-8\"></head><body><h1>Ask</h1></body></html>";
        let built = build_agent_document(html, false, &vars());
        assert!(built.contains("<h1>Ask</h1>"));
        assert!(built.starts_with("<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\""));
        assert!(built.find("window.cod").unwrap() < built.find("<meta charset").unwrap());
    }

    #[test]
    fn the_bootstrap_script_runs_before_the_pages_own_scripts() {
        // A page script that calls window.cod while the document loads must find it defined (#43 AC4).
        let html = "<!doctype html><html><head><script>var early = 1;</script></head><body><script>window.cod.call('submit', {value: 'x'});</script></body></html>";
        let built = build_agent_document(html, true, &vars());
        let bootstrap = built.find("window.cod={").unwrap();
        assert!(bootstrap < built.find("var early").unwrap());
        assert!(bootstrap < built.find("window.cod.call('submit'").unwrap());
    }

    // RC review B1: Cod's prelude is now a complete, fixed document of its own, always first,
    // regardless of whatever doctype, `<html>` or `<head>` tags the page's own source carries.
    #[test]
    fn cods_prelude_always_comes_first_no_matter_what_the_pages_own_markup_starts_with() {
        const PRELUDE: &str = "<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\"";
        assert!(build_agent_document("<p>hi</p>", false, &vars()).starts_with(PRELUDE));
        assert!(build_agent_document("<!DOCTYPE html><p>hi</p>", false, &vars()).starts_with(PRELUDE));
        assert!(
            build_agent_document("<HTML lang=\"en\"><Body><p>hi</p></Body></HTML>", false, &vars())
                .starts_with(PRELUDE)
        );
        assert!(
            build_agent_document("<html><header>x</header></html>", false, &vars())
                .starts_with(PRELUDE)
        );
        // The page's own markup, comment included, survives unchanged after Cod's prelude.
        let built = build_agent_document("<!DOCTYPE html><p>hi</p>", false, &vars());
        assert!(built.contains("<!DOCTYPE html><p>hi</p>"));
    }

    // RC review B1: a page that hides its own `<head>` inside a comment used to fool the old
    // splice-based injection into placing the CSP and bootstrap inside that comment, where
    // neither ever took effect; an onerror handler could then forge a bridge call freely.
    #[test]
    fn a_head_tag_hidden_inside_a_comment_does_not_capture_cods_prelude() {
        let html = "<!-- <head> --><body><img src=\"data:,\" onerror=\"window.cod.call('submit',{value:'forged'})\"></body>";
        let built = build_agent_document(html, false, &vars());
        assert!(built.starts_with("<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\""));
        let head_close = built.find("</head>").expect("Cod's own </head>");
        let comment = built.find("<!--").expect("the page's own comment, unchanged");
        assert!(
            head_close < comment,
            "Cod's prelude must close before the page's own comment starts: {built}"
        );
    }

    #[test]
    fn js_is_kept_only_with_allow_js() {
        let html = "<body><script>window.ranInlineScript = true;</script></body>";
        assert!(!build_agent_document(html, false, &vars()).contains("ranInlineScript"));
        assert!(build_agent_document(html, true, &vars()).contains("ranInlineScript"));
    }

    // RC review B1: stripping `<script>` elements alone does not stop an inline event handler
    // (`onerror`, `onclick`, ...) from running. Without `--js`, the built document must carry a
    // Content-Security-Policy that allows only Cod's own, freshly nonced bootstrap script; a
    // real browser engine enforcing it is what actually blocks the handler (checked end to end
    // in tests/e2e/test_43_agent_pages.py), but the document itself must be built right first.

    #[test]
    fn without_js_the_document_carries_a_csp_naming_cods_own_nonce() {
        let html = "<body><img src=\"data:,\" onerror=\"window.cod.call('submit',{value:'forged'})\"></body>";
        let built = build_agent_document(html, false, &vars());
        assert!(built.contains("Content-Security-Policy"));
        let nonce = built
            .split("script-src 'nonce-")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .expect("a nonce in the CSP")
            .to_owned();
        assert!(!nonce.is_empty());
        // Cod's own bootstrap carries the exact nonce the CSP names; the page's own handler, left
        // untouched as plain text, carries none and so has nothing to match.
        assert!(built.contains(&format!("<script nonce=\"{nonce}\">")));
        assert!(built.contains("onerror=\"window.cod.call('submit',{value:'forged'})\""));
        assert!(!built.contains(&format!("onerror=\"window.cod.call('submit',{{value:'forged'}})\" nonce=\"{nonce}\"")));
    }

    #[test]
    fn two_documents_never_share_a_csp_nonce() {
        let first = build_agent_document("<p>a</p>", false, &vars());
        let second = build_agent_document("<p>a</p>", false, &vars());
        assert_ne!(first, second);
    }

    #[test]
    fn allow_js_adds_no_content_security_policy() {
        let built = build_agent_document("<p>hi</p>", true, &vars());
        assert!(!built.contains("Content-Security-Policy"));
    }

    // RC review residual (B2 for `--js` pages): bridge calls are matched to their document by a
    // random token in Cod's bootstrap, not by the console source CEF reports.
    #[test]
    fn the_document_token_round_trips_through_the_data_url_with_and_without_js() {
        for allow_js in [true, false] {
            let built = build_agent_document("<p>hi</p>", allow_js, &vars());
            let token = document_token(&data_url(&built)).expect("a token in Cod's bootstrap");
            assert!(token.len() >= 20);
            assert!(built.contains(&format!("{TOKEN_MARKER}{token}'")));
        }
    }

    #[test]
    fn a_token_written_by_the_page_itself_is_never_the_documents_token() {
        let page = "<p>,t:'forged'</p><script>console.log(\",t:'forged'\")</script>";
        let built = build_agent_document(page, true, &vars());
        let token = document_token(&data_url(&built)).expect("a token in Cod's bootstrap");
        assert_ne!(token, "forged");
    }

    #[test]
    fn two_documents_never_share_a_token() {
        let first = document_token(&data_url(&build_agent_document("<p>a</p>", true, &vars())));
        let second = document_token(&data_url(&build_agent_document("<p>a</p>", true, &vars())));
        assert_ne!(first, second);
    }

    // RC delta review: agent pages saved before tokens existed must keep a working bridge after
    // an upgrade, with or without the CSP they were opened with.
    #[test]
    fn a_legacy_agent_document_is_upgraded_to_a_tokened_one() {
        let legacy_prelude = "<script nonce=\"n\">(function(){window.cod={call:codSend};})();</script>";
        let no_js = format!(
            "<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'nonce-n'\">{legacy_prelude}</head><p>decide</p>"
        );
        let upgraded = upgrade_legacy_document(&data_url(&no_js), &vars()).expect("an upgrade");
        assert!(document_token(&upgraded).is_some());
        let decoded = decode(&upgraded);
        assert!(decoded.contains("Content-Security-Policy"));
        assert!(decoded.ends_with("<p>decide</p>"));

        let with_js = format!("<!doctype html><html><head>{legacy_prelude}</head><p>decide</p>");
        let upgraded = upgrade_legacy_document(&data_url(&with_js), &vars()).expect("an upgrade");
        assert!(document_token(&upgraded).is_some());
        assert!(!decode(&upgraded).contains("Content-Security-Policy"));
    }

    #[test]
    fn only_legacy_agent_documents_are_upgraded() {
        let current = data_url(&build_agent_document("<p>a</p>", false, &vars()));
        assert_eq!(upgrade_legacy_document(&current, &vars()), None);
        let foreign = data_url("<!doctype html><html><head><title>x</title></head><p>a</p>");
        assert_eq!(upgrade_legacy_document(&foreign, &vars()), None);
    }

    fn decode(url: &str) -> String {
        use base64::Engine as _;
        let encoded = url.strip_prefix("data:text/html;charset=utf-8;base64,").unwrap();
        String::from_utf8(base64::engine::general_purpose::STANDARD.decode(encoded).unwrap()).unwrap()
    }

    #[test]
    fn a_bridge_call_carries_its_token() {
        let call = parse_bridge_message(r#"cod:bridge:{"name":"submit","args":{},"t":"abc"}"#)
            .expect("a bridge call");
        assert_eq!(call.token.as_deref(), Some("abc"));
        let call = parse_bridge_message(r#"cod:bridge:{"name":"submit","args":{}}"#)
            .expect("a bridge call");
        assert_eq!(call.token, None);
    }

    #[test]
    fn data_urls_decode_back_to_the_same_html() {
        use base64::Engine as _;
        let html = "<p>hi</p>";
        let url = data_url(html);
        let encoded = url.strip_prefix("data:text/html;charset=utf-8;base64,").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), html);
    }

    #[test]
    fn theme_variables_are_set_as_javascript_string_literals() {
        let script = theme_update_script(&vars());
        assert!(script.contains("--cod-background"));
        assert!(script.contains("'rgba(10, 10, 10, 1.000)'"));
        assert!(script.contains("Zed Sans"));
    }

    fn page(id: &str) -> PageId {
        PageId(id.to_owned().into())
    }

    #[test]
    fn a_submit_is_answered_only_once_the_page_is_gone() {
        let mut waiters = Waiters::default();
        let mut receiver = waiters.register(page("page-1"));
        waiters.record_submit(&page("page-1"), "half-even".into());
        assert_eq!(receiver.try_recv(), Ok(None));
        waiters.finish(&page("page-1"));
        assert_eq!(receiver.try_recv(), Ok(Some(SubmitOutcome::Submitted("half-even".into()))));
    }

    #[test]
    fn a_page_closed_without_a_submit_answers_closed() {
        let mut waiters = Waiters::default();
        let mut receiver = waiters.register(page("page-2"));
        waiters.record_submit(&page("page-3"), "other page".into());
        waiters.finish(&page("page-2"));
        assert_eq!(receiver.try_recv(), Ok(Some(SubmitOutcome::Closed)));
    }

    #[test]
    fn javascript_string_literals_escape_quotes_and_backslashes() {
        assert_eq!(js_string("a'b\\c"), "'a\\'b\\\\c'");
    }
}
