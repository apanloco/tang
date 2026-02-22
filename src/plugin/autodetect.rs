use super::PluginType;

#[cfg(feature = "lv2")]
fn lv2_resolve(source: String) -> anyhow::Result<(PluginType, String)> {
    Ok((PluginType::Lv2, source))
}

#[cfg(not(feature = "lv2"))]
fn lv2_resolve(_source: String) -> anyhow::Result<(PluginType, String)> {
    anyhow::bail!("LV2 support is not enabled (compile with --features lv2)")
}

/// Resolve a plugin source string into a plugin type and normalized source.
///
/// Supported formats:
///   - `lv2:<URI>`              — explicit LV2 URI
///   - `clap:<ID>`              — explicit CLAP ID
///   - `path/to/foo.lv2`       — LV2 bundle path
///   - `path/to/foo.clap`      — CLAP bundle path
///   - `http://…` / `urn:…`    — auto-detected as LV2 URI
///   - `com.vendor.plugin`     — auto-detected as CLAP reverse-domain ID
pub fn resolve(source: &str) -> anyhow::Result<(PluginType, String)> {
    // Explicit prefixes
    if source.starts_with("lv2:") {
        return lv2_resolve(source.to_string());
    }
    if source.starts_with("clap:") {
        return Ok((PluginType::Clap, source.to_string()));
    }

    // File path extensions
    if source.ends_with(".lv2") || source.ends_with(".lv2/") {
        return lv2_resolve(source.to_string());
    }
    if source.ends_with(".clap") || source.ends_with(".clap/") {
        return Ok((PluginType::Clap, source.to_string()));
    }

    // Auto-detect LV2 URI
    if source.starts_with("http://") || source.starts_with("https://") || source.starts_with("urn:")
    {
        return lv2_resolve(format!("lv2:{source}"));
    }

    // Auto-detect CLAP reverse-domain ID (contains dots, no path separators)
    if source.contains('.') && !source.contains('/') {
        return Ok((PluginType::Clap, format!("clap:{source}")));
    }

    anyhow::bail!(
        "Unknown plugin format: {source}\n\
         Expected one of:\n  \
           http://…               (LV2 URI)\n  \
           com.vendor.plugin      (CLAP ID)\n  \
           lv2:<URI>              (explicit LV2)\n  \
           clap:<ID>              (explicit CLAP)\n  \
           /path/to/plugin.lv2\n  \
           /path/to/plugin.clap\n\
         Run `tang enumerate plugins` to list available plugins."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "lv2")]
    #[test]
    fn explicit_lv2_prefix() {
        let (ty, src) = resolve("lv2:http://tytel.org/helm").unwrap();
        assert_eq!(ty, PluginType::Lv2);
        assert_eq!(src, "lv2:http://tytel.org/helm");
    }

    #[test]
    fn explicit_clap_prefix() {
        let (ty, src) = resolve("clap:com.u-he.diva").unwrap();
        assert_eq!(ty, PluginType::Clap);
        assert_eq!(src, "clap:com.u-he.diva");
    }

    #[cfg(feature = "lv2")]
    #[test]
    fn lv2_bundle_path() {
        let (ty, src) = resolve("/usr/lib/lv2/helm.lv2").unwrap();
        assert_eq!(ty, PluginType::Lv2);
        assert_eq!(src, "/usr/lib/lv2/helm.lv2");
    }

    #[cfg(feature = "lv2")]
    #[test]
    fn lv2_bundle_path_trailing_slash() {
        let (ty, src) = resolve("/usr/lib/lv2/helm.lv2/").unwrap();
        assert_eq!(ty, PluginType::Lv2);
        assert_eq!(src, "/usr/lib/lv2/helm.lv2/");
    }

    #[test]
    fn clap_bundle_path() {
        let (ty, src) = resolve("/usr/lib/clap/diva.clap").unwrap();
        assert_eq!(ty, PluginType::Clap);
        assert_eq!(src, "/usr/lib/clap/diva.clap");
    }

    #[cfg(feature = "lv2")]
    #[test]
    fn bare_http_uri() {
        let (ty, src) = resolve("http://tytel.org/helm").unwrap();
        assert_eq!(ty, PluginType::Lv2);
        assert_eq!(src, "lv2:http://tytel.org/helm");
    }

    #[cfg(feature = "lv2")]
    #[test]
    fn bare_https_uri() {
        let (ty, src) = resolve("https://example.org/plugin").unwrap();
        assert_eq!(ty, PluginType::Lv2);
        assert_eq!(src, "lv2:https://example.org/plugin");
    }

    #[cfg(feature = "lv2")]
    #[test]
    fn bare_urn() {
        let (ty, src) = resolve("urn:lv2:some-plugin").unwrap();
        assert_eq!(ty, PluginType::Lv2);
        assert_eq!(src, "lv2:urn:lv2:some-plugin");
    }

    #[test]
    fn bare_clap_id() {
        let (ty, src) = resolve("com.u-he.diva").unwrap();
        assert_eq!(ty, PluginType::Clap);
        assert_eq!(src, "clap:com.u-he.diva");
    }

    #[test]
    fn bare_clap_id_deep() {
        let (ty, src) = resolve("org.surge-synth-team.surge-xt").unwrap();
        assert_eq!(ty, PluginType::Clap);
        assert_eq!(src, "clap:org.surge-synth-team.surge-xt");
    }

    #[test]
    fn unknown_format() {
        assert!(resolve("something-without-dots").is_err());
    }

    #[cfg(not(feature = "lv2"))]
    #[test]
    fn lv2_disabled_error() {
        let err = resolve("lv2:http://tytel.org/helm").unwrap_err();
        assert!(
            err.to_string().contains("LV2 support is not enabled"),
            "unexpected error: {err}"
        );
    }
}
