//! Shared presentation for the public guide, console, and device approval page.
pub(crate) fn shell(mut html: String) -> String {
    for tag in ["header", "footer"] {
        if let Some(start) = html.find(&format!("<{tag}>"))
            && let Some(end) = html[start..].find(&format!("</{tag}>"))
        {
            html.replace_range(start..start + end + tag.len() + 3, "");
        }
    }
    // Older guide templates omit explicit head/body tags; normalize those too.
    if !html.contains("<head>") {
        html = html.replacen("<meta", "<head><meta", 1);
    }
    if !html.contains("</head>") {
        html = html.replacen("<main", "</head><body><main", 1);
    }
    if !html.contains("</body>") {
        html = html.replace("</html>", "</body></html>");
    }
    html = html.replacen("</head>", "<link rel=\"stylesheet\" href=\"/flower-site.css\"><link rel=\"stylesheet\" href=\"/flower-header.css\"><link rel=\"stylesheet\" href=\"/flower-footer.css\"><link rel=\"stylesheet\" href=\"/cloud.css\"><script src=\"/site.js\" defer></script></head>", 1);
    html = html.replacen(
        "<body>",
        &format!(
            "<body class=\"site-body\"><div class=\"site-shell\">{}",
            include_str!("../static/header.html")
        ),
        1,
    );
    if !html.contains("id=\"main-content\"") {
        html = html.replacen("<main", "<main id=\"main-content\"", 1);
    }
    html.replace(
        "</body>",
        &format!("{}</div></body>", include_str!("../static/footer.html")),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_pages_have_one_navigation_and_preserve_form_targets() {
        for source in [
            include_str!("../static/home.html"),
            include_str!("../static/console.html"),
            include_str!("../static/device.html"),
            include_str!("../static/index.html"),
        ] {
            let html = super::shell(source.to_owned());
            for needle in [
                "id=\"logout\"",
                "id=\"main-content\"",
                "class=\"site-header\"",
                "<head>",
                "</head>",
                "</body>",
            ] {
                assert_eq!(
                    html.matches(needle).count(),
                    1,
                    "duplicate or missing {needle}"
                );
            }
            assert!(html.contains("/flower-site.css"));
            assert!(html.contains("/site.js"));
            if source.contains("id=\"create-workspace\"") {
                assert!(html.contains("id=\"create-workspace\""));
            }
            if source.contains("id=\"approve\"") {
                assert!(html.contains("id=\"approve\""));
            }
        }
    }
}
