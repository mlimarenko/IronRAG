use super::{
    extract_html_main_content, extract_html_preferred_url, payload_looks_like_html_document,
};

mod media;

#[test]
fn detects_html_payload_by_prefix() {
    assert!(payload_looks_like_html_document("<!DOCTYPE html><html><body>Hello</body></html>"));
    assert!(!payload_looks_like_html_document("plain text payload"));
}

#[test]
fn extracts_main_content_and_links_from_html() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head>
            <title>Docs Home</title>
          </head>
          <body>
            <nav>Top navigation</nav>
            <main>
              <h1>IronRAG Docs</h1>
              <p>Ship one ingestion path.</p>
              <ul><li>Single page</li><li>Recursive crawl</li></ul>
              <a href="/guide">Guide</a>
              <a href="https://example.org/outside">Outside</a>
            </main>
            <footer>Footer</footer>
          </body>
        </html>
    "#;

    let output = extract_html_main_content(html.as_bytes(), Some("text/html; charset=utf-8"))
        .expect("html extraction");

    assert_eq!(output.extraction_kind, "html_main_content");
    assert_eq!(
        output.source_map.get("title").and_then(serde_json::Value::as_str),
        Some("Docs Home")
    );
    assert!(output.content_text.contains("# IronRAG Docs"));
    assert!(output.content_text.contains("Ship one ingestion path."));
    assert!(!output.content_text.contains("Top navigation"));
    assert!(!output.content_text.contains("Footer"));
    assert_eq!(
        output
            .source_map
            .get("outboundLinks")
            .and_then(serde_json::Value::as_array)
            .map(std::vec::Vec::len),
        Some(2)
    );
}

#[test]
fn title_alone_does_not_create_readable_html_content() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Sign In</title></head>
          <body>
            <form>
              <input name="email" />
              <input name="password" />
            </form>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.trim().is_empty());
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| warning == "html page did not yield readable main content")
    );
}

#[test]
fn short_visible_body_content_keeps_page_title_context() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Status Page</title></head>
          <body>
            <main>
              <p>Version 42 is available.</p>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("# Status Page"));
    assert!(output.content_text.contains("Version 42 is available."));
}

#[test]
fn scopes_outbound_links_to_selected_formal_content_root() {
    let html = r##"
        <!DOCTYPE html>
        <html>
          <head><title>Article Page</title></head>
          <body>
            <nav><a href="/account/login">Login</a></nav>
            <article>
              <div>
                <p>Article body.</p>
                <a href="/docs/topic">Topic</a>
                <a href="/docs/reference">Reference</a>
                <a href="#local">Local</a>
              </div>
            </article>
            <footer><a href="/privacy">Privacy</a></footer>
          </body>
        </html>
    "##;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert_eq!(
        output.source_map.get("outboundLinks"),
        Some(&serde_json::json!(["/docs/reference", "/docs/topic"]))
    );
    assert!(!output.content_text.contains("Login"));
    assert!(!output.content_text.contains("Privacy"));
}

#[test]
fn preserves_http_links_regardless_of_query_names() {
    let html = r##"
        <!DOCTYPE html>
        <html>
          <head><title>Article Page</title></head>
          <body>
            <main>
              <p>Article body.</p>
              <a href="/w/index.php?title=Article&action=edit&section=1">edit</a>
              <a href="https://editor.example.test/page?veaction=edit">visual edit</a>
              <a href="/docs/topic?mode=read">Topic</a>
            </main>
          </body>
        </html>
    "##;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(
        output.content_text.contains("[edit](/w/index.php?title=Article&action=edit&section=1)")
    );
    assert!(
        output
            .content_text
            .contains("[visual edit](https://editor.example.test/page?veaction=edit)")
    );
    assert_eq!(
        output.source_map.get("outboundLinks"),
        Some(&serde_json::json!([
            "/docs/topic?mode=read",
            "/w/index.php?title=Article&action=edit&section=1",
            "https://editor.example.test/page?veaction=edit"
        ]))
    );
}

#[test]
fn cms_selector_names_do_not_override_formal_article_root() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Article Page</title></head>
          <body>
            <div id="main-content" class="wiki-content">
              <a href="/account/login">CMS selector decoy</a>
            </div>
            <article>
            <div class="mw-parser-output">
              <p>This article body has enough readable content to outrank a short account link block inside another high-confidence container.</p>
              <a href="/docs/topic">Topic</a>
            </div>
            </article>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("This article body has enough readable content"));
    assert!(!output.content_text.contains("CMS selector decoy"));
    assert_eq!(output.source_map.get("outboundLinks"), Some(&serde_json::json!(["/docs/topic"])));
}

#[test]
fn excludes_script_style_and_hidden_descendant_text_from_blocks() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Clean Article</title></head>
          <body>
            <main>
              <article>
                <style>.card { color: red; }</style>
                <script>window.app = {"debug": true};</script>
                <p>Readable article body.</p>
                <span hidden>Invisible template copy</span>
              </article>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("Readable article body."));
    assert!(!output.content_text.contains(".card"));
    assert!(!output.content_text.contains("window.app"));
    assert!(!output.content_text.contains("Invisible template copy"));
}

#[test]
fn arbitrary_class_names_do_not_hide_content() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Visibility</title></head>
          <body>
            <main>
              <p class="visually-hidden">Screen reader duplicate heading</p>
              <p style="display: none !important">Collapsed duplicate text</p>
              <p style="visibility: collapse">Collapsed table text</p>
              <p>Visible body text.</p>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("Visible body text."));
    assert!(output.content_text.contains("Screen reader duplicate heading"));
    assert!(!output.content_text.contains("Collapsed duplicate text"));
    assert!(!output.content_text.contains("Collapsed table text"));
}

#[test]
fn drops_template_dominated_text_but_preserves_template_prose() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Template Text</title></head>
          <body>
            <main>
              <p>{{ locale ? title.localized : title.default }}</p>
              <p>- {{ firstOption }} - {{ secondOption }}</p>
              <p>Use {{ value }} when documenting template examples in prose.</p>
              <p>Pass <code>{{ inline_code_sample }}</code> as the parameter value.</p>
              <pre>{{ preserved_code_sample }}</pre>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(!output.content_text.contains("title.localized"));
    assert!(!output.content_text.contains("firstOption"));
    assert!(
        output
            .content_text
            .contains("Use {{ value }} when documenting template examples in prose.")
    );
    assert!(output.content_text.contains("{{ inline_code_sample }}"));
    assert!(output.content_text.contains("{{ preserved_code_sample }}"));
}

#[test]
fn drops_template_dominated_text_with_small_separator_labels() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Template Chrome</title></head>
          <body>
            <main>
              <p>or {{ firstOption }} / {{ secondOption }}</p>
              <p>Use {{ value }} when documenting template examples in prose.</p>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(!output.content_text.contains("firstOption"));
    assert!(
        output
            .content_text
            .contains("Use {{ value }} when documenting template examples in prose.")
    );
}

#[test]
fn prefers_formal_main_content_over_outer_container() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head>
            <title>Acme Software Products</title>
          </head>
          <body>
            <div id="content">
              <div class="page-metadata">Created by Alice</div>
              <main>
                <div class="contentLayout2">
                  <div class="columnLayout">
                    <div class="cell">
                      <a href="/pages/viewpage.action?pageId=1">
                        <img src="/download/attachments/1/POS.png" />
                      </a>
                    </div>
                    <div class="cell">
                      <a href="/x/2">
                        <img src="/download/attachments/1/hybrid_pos.png" />
                      </a>
                    </div>
                  </div>
                </div>
              </main>
              <div id="labels-section">No labels</div>
            </div>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("confluence html");

    assert!(output.content_text.contains("# Acme Software Products"));
    assert!(output.content_text.contains("POS"));
    assert!(output.content_text.contains("Hybrid Pos"));
    assert!(!output.content_text.contains("Created by Alice"));
    assert!(!output.content_text.contains("No labels"));
}

#[test]
fn extracts_html_preferred_url_against_base_url() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head>
            <link rel="canonical" href="/pages/viewpage.action?pageId=44597523" />
          </head>
          <body>
            <main>Docs home</main>
          </body>
        </html>
    "#;

    let preferred_url = extract_html_preferred_url(
        html.as_bytes(),
        Some("text/html"),
        "https://docs.example.test/",
    );

    assert_eq!(
        preferred_url.as_deref(),
        Some("https://docs.example.test/pages/viewpage.action?pageId=44597523")
    );
}

#[test]
fn prefers_dense_formal_main_content_even_when_outer_container_has_more_chrome() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head>
            <title>Acme Operations Console</title>
          </head>
          <body>
            <div id="content">
              <div class="page-metadata">Created by Alice</div>
              <div id="page-metadata-banner">3 attachments</div>
              <div id="breadcrumbs">Docs / Products / Operations Console</div>
              <main>
                <h1>Acme Operations Console</h1>
                <p>Operations Console is used to manage distributed workflows.</p>
                <p>It centralizes settings, notifications, and remote administration flows.</p>
              </main>
            </div>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("confluence html");

    assert!(output.content_text.contains("Operations Console is used to manage"));
    assert!(!output.content_text.contains("Created by Alice"));
    assert!(!output.content_text.contains("3 attachments"));
    assert!(!output.content_text.contains("Docs / Products / Operations Console"));
}
