use super::extract_html_main_content;

#[test]
fn extracts_meaningful_image_surrogates_and_resources_from_main_content() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Widget Guide</title></head>
          <body>
            <header>
              <img src="/assets/header-badge.png" width="16" height="16" alt="" />
            </header>
            <main>
              <h1>Widget Guide</h1>
              <figure>
                <img
                  src="/docs/widget-flow.png"
                  width="960"
                  height="540"
                  alt="Widget configuration flow"
                />
                <figcaption>Configuration flow from upload to approval.</figcaption>
              </figure>
              <img src="data:image/png;base64,AA==" alt="inline pixel" />
              <img src="/docs/tiny.png" width="8" height="8" />
              <img src="/docs/vector.svg" width="640" height="480" alt="Vector diagram" />
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("# Widget Guide"));
    assert!(
        output
            .content_text
            .contains("![Configuration flow from upload to approval.](/docs/widget-flow.png)")
    );
    assert!(!output.content_text.contains("header-badge"));
    assert!(!output.content_text.contains("inline pixel"));
    assert!(!output.content_text.contains("tiny.png"));
    assert!(!output.content_text.contains("vector.svg"));

    let resources = output
        .source_map
        .get("outboundResources")
        .and_then(serde_json::Value::as_array)
        .expect("outbound resources");
    assert_eq!(resources, &vec![serde_json::json!("/docs/widget-flow.png")]);
}

#[test]
fn ignores_image_resources_inside_chrome_when_body_is_content_root() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Body Fallback</title></head>
          <body>
            <header>
              <img src="/chrome/company-logo.png" width="300" height="120" alt="Company logo" />
            </header>
            <article>
              <h1>Body Fallback</h1>
              <p>Documentation page without an explicit main element.</p>
              <img src="/docs/architecture.png" width="960" height="540" alt="Architecture diagram" />
            </article>
            <footer>
              <img src="/chrome/footer-badge.png" width="300" height="120" alt="Footer badge" />
            </footer>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("![Architecture diagram](/docs/architecture.png)"));
    assert!(!output.content_text.contains("company-logo"));
    assert!(!output.content_text.contains("footer-badge"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["/docs/architecture.png"]))
    );
}

#[test]
fn preserves_text_around_inline_images() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Inline Figure</title></head>
          <body>
            <main>
              <div>
                Introductory context
                <img src="/docs/inline-flow.png" width="640" height="360" alt="Inline flow" />
                Closing context
              </div>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("Introductory context Closing context"));
    assert!(output.content_text.contains("![Inline flow](/docs/inline-flow.png)"));
}

#[test]
fn extracts_best_srcset_candidate_when_image_has_no_src() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Diagram Page</title></head>
          <body>
            <main>
              <img
                srcset="/images/diagram-small.webp 1x, /images/diagram-large.webp 2x"
                width="400"
                height="300"
                alt="Deployment diagram"
              />
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("![Deployment diagram](/images/diagram-large.webp)"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["/images/diagram-large.webp"]))
    );
}

#[test]
fn prefers_higher_quality_srcset_candidate_over_src_thumbnail() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Screenshot Page</title></head>
          <body>
            <main>
              <img
                src="/images/screenshot-250.png"
                srcset="/images/screenshot-250.png 250w, /images/screenshot-750.png 750w"
                width="250"
                height="160"
                alt="Settings screenshot"
              />
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("![Settings screenshot](/images/screenshot-750.png)"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["/images/screenshot-750.png"]))
    );
}

#[test]
fn accepts_protocol_relative_srcset_images_with_structural_size_signal() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Protocol Relative</title></head>
          <body>
            <main>
              <img
                src="//cdn.example.test/thumbs/diagram.png"
                srcset="//cdn.example.test/thumbs/diagram-large.png 2x"
                width="250"
                height="160"
              />
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(
        output
            .content_text
            .contains("![Diagram Large](//cdn.example.test/thumbs/diagram-large.png)")
    );
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["//cdn.example.test/thumbs/diagram-large.png"]))
    );
}

#[test]
fn uses_lazy_image_source_when_src_is_placeholder() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Lazy Image</title></head>
          <body>
            <main>
              <img
                src="data:image/gif;base64,R0lGODlhAQABAAAAACw="
                data-src="/screenshots/checkout-settings.png"
                width="900"
                height="520"
                alt="Checkout settings"
              />
              <img
                data-srcset="/screenshots/audit-small.webp 320w, /screenshots/audit-large.webp 960w"
                width="480"
                height="280"
                alt="Audit flow"
              />
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(
        output.content_text.contains("![Checkout settings](/screenshots/checkout-settings.png)")
    );
    assert!(output.content_text.contains("![Audit flow](/screenshots/audit-large.webp)"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!([
            "/screenshots/audit-large.webp",
            "/screenshots/checkout-settings.png"
        ]))
    );
}

#[test]
fn prefers_lazy_original_over_empty_placeholder_src() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Lazy Original</title></head>
          <body>
            <main>
              <img
                src="https://cdn.example.test/assets/-/empty/screenshot.png"
                data-original="https://cdn.example.test/assets/screenshot.png"
                alt="Builder screenshot"
              />
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(
        output
            .content_text
            .contains("![Builder screenshot](https://cdn.example.test/assets/screenshot.png)")
    );
    assert!(!output.content_text.contains("/-/empty/screenshot.png"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["https://cdn.example.test/assets/screenshot.png"]))
    );
}

#[test]
fn extracts_raster_background_images_without_svg_chrome() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Visual Section</title></head>
          <body>
            <main>
              <section style="background-image:url('https://cdn.example.test/screens/app.jpg')">
                App launch screen
              </section>
              <section style="background-image:url('https://cdn.example.test/chrome/wave.svg')">
                Decorative wave
              </section>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(
        output
            .content_text
            .contains("![App launch screen](https://cdn.example.test/screens/app.jpg)")
    );
    assert!(!output.content_text.contains("wave.svg"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["https://cdn.example.test/screens/app.jpg"]))
    );
}

#[test]
fn ignores_unlabeled_background_images_as_decorative() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Decorative Background</title></head>
          <body>
            <main>
              <section style="background-image:url('https://cdn.example.test/patterns/dots.png')"></section>
              <p>Readable article body.</p>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("Readable article body."));
    assert!(!output.content_text.contains("dots.png"));
    assert_eq!(output.source_map.get("outboundResources"), Some(&serde_json::json!([])));
}

#[test]
fn ignores_hidden_images_and_keeps_visible_pair() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Product Grid</title></head>
          <body>
            <main>
              <img src="/cards/register.png" alt="Register card" />
              <img src="/cards/register-hover.png" style="display: none;" alt="Register hover" />
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("![Register card](/cards/register.png)"));
    assert!(!output.content_text.contains("register-hover"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["/cards/register.png"]))
    );
}

#[test]
fn extracts_picture_source_with_nested_image_label() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Responsive Media</title></head>
          <body>
            <main>
              <picture>
                <source srcset="/media/report-small.webp 320w, /media/report-large.webp 960w" />
                <img src="/media/report-fallback.png" alt="Report dashboard" />
              </picture>
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("![Report dashboard](/media/report-large.webp)"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["/media/report-large.webp"]))
    );
}

#[test]
fn ignores_unsupported_srcset_candidate_without_discarding_supported_images() {
    let html = r#"
        <!DOCTYPE html>
        <html>
          <head><title>Mixed Srcset</title></head>
          <body>
            <main>
              <img
                src="/screenshots/fallback.png"
                srcset="/screenshots/readable.png 640w, /screenshots/readable.avif 1920w"
                width="640"
                height="360"
                alt="Readable screenshot"
              />
            </main>
          </body>
        </html>
    "#;

    let output =
        extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

    assert!(output.content_text.contains("![Readable screenshot](/screenshots/readable.png)"));
    assert_eq!(
        output.source_map.get("outboundResources"),
        Some(&serde_json::json!(["/screenshots/readable.png"]))
    );
}
