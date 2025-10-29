use crate::config::{
    Config, OutputOptions, PackAlgorithm, PackMode, PackOptions, PackSort, StaticOptions,
};
use anyhow::{Context, Result};
use fs_err as fs;
use serde::Deserialize;
use std::path::Path;

/// Old flat pack configuration structure (pre-v2.0)
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct OldPackOptions {
    pub enabled: bool,
    // Static-specific fields (were top-level before)
    pub max_size: (u32, u32),
    pub power_of_two: bool,
    pub padding: u32,
    pub extrude: u32,
    pub allow_trim: bool,
    pub algorithm: PackAlgorithm,
    pub page_limit: Option<u32>,
    pub sort: PackSort,
    pub dedupe: bool,
}

impl Default for OldPackOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            max_size: (2048, 2048),
            power_of_two: true,
            padding: 2,
            extrude: 1,
            allow_trim: false,
            algorithm: PackAlgorithm::MaxRects,
            page_limit: None,
            sort: PackSort::Area,
            dedupe: false,
        }
    }
}

impl OldPackOptions {
    /// Convert old flat structure to new PackMode::Static
    pub fn to_new_format(&self) -> PackOptions {
        PackOptions {
            enabled: self.enabled,
            output: OutputOptions {
                name: None,
                overwrite: false,
            },
            mode: PackMode::Static(StaticOptions {
                max_size: self.max_size,
                power_of_two: self.power_of_two,
                padding: self.padding,
                extrude: self.extrude,
                allow_trim: self.allow_trim,
                algorithm: self.algorithm.clone(),
                page_limit: self.page_limit,
                sort: self.sort.clone(),
                dedupe: self.dedupe,
            }),
        }
    }
}

/// Migrate configuration file from old to new format
pub fn migrate_config(
    input_path: &str,
    output_path: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    let input = Path::new(input_path);
    let output = output_path.map(Path::new).unwrap_or(input);

    // Read and parse input file
    let content = fs::read_to_string(input)
        .with_context(|| format!("Failed to read config from {}", input.display()))?;

    // Try to parse as new format first
    if let Ok(_new_config) = parse_new_config(&content) {
        if !force {
            anyhow::bail!(
                "Configuration file '{}' is already in new format (v2.0). Use --force to convert anyway.",
                input.display()
            );
        }
        log::warn!("Config appears to be new format, but forcing conversion");
    }

    // Parse as old format and convert
    let old_config = parse_old_config(&content)
        .with_context(|| format!("Failed to parse old config from {}", input.display()))?;

    let new_config = convert_config(old_config)?;

    // Convert back to Config struct to ensure proper serialization
    let config: Config = serde_json::from_value(new_config)
        .context("Failed to convert migrated config to Config struct")?;

    // Determine output format based on file extension
    let output_content = match output.extension().and_then(|s| s.to_str()) {
        Some("json") | Some("json5") | Some("jsonc") => serde_json::to_string_pretty(&config)
            .context("Failed to serialize new config to JSON")?,
        Some("toml") | None => {
            toml::to_string_pretty(&config).context("Failed to serialize new config to TOML")?
        }
        Some(ext) => {
            anyhow::bail!("Unsupported file extension: {}", ext);
        }
    };

    if dry_run {
        println!("=== Dry run: would write to {} ===", output.display());
        println!("{}", output_content);
        println!("\n=== Differences from current format ===");
        println!("- Flat pack options converted to PackMode::Static");
        println!("- Added OutputOptions with defaults");
        return Ok(());
    }

    // Create backup if output path is the same as input
    if output == input {
        let backup_path = input.with_extension(format!(
            "{}.old",
            input.extension().and_then(|s| s.to_str()).unwrap_or("toml")
        ));
        fs::copy(input, &backup_path)
            .with_context(|| format!("Failed to create backup at {}", backup_path.display()))?;
        log::info!("Created backup at {}", backup_path.display());
    }

    // Write new config
    fs::write(output, &output_content)
        .with_context(|| format!("Failed to write new config to {}", output.display()))?;

    log::info!("Config file written to {}", output.display());
    log::info!("Note: Use 'asphalt check' to validate the new configuration");

    log::info!("Successfully migrated config to {}", output.display());
    println!("✓ Config migrated successfully");
    println!("  Old format: flat pack options");
    println!("  New format: PackMode::Static with OutputOptions");

    Ok(())
}

fn parse_new_config(content: &str) -> Result<Config> {
    // Try all supported formats
    serde_json::from_str(content)
        .or_else(|_| json5::from_str(content))
        .or_else(|_| toml::from_str(content))
        .context("Failed to parse as new config format")
}

fn parse_old_config(content: &str) -> Result<serde_json::Value> {
    // Parse as generic JSON/TOML value first
    serde_json::from_str(content)
        .or_else(|_| json5::from_str(content))
        .or_else(|_| toml::from_str(content))
        .context("Failed to parse config file")
}

fn convert_config(mut old_value: serde_json::Value) -> Result<serde_json::Value> {
    // Work directly with JSON values to avoid Config serialization issues
    // For each input with pack settings, convert from old to new format
    if let Some(obj) = old_value.as_object_mut() {
        if let Some(inputs) = obj.get_mut("inputs") {
            if let Some(inputs_obj) = inputs.as_object_mut() {
                for (input_name, input_value) in inputs_obj.iter_mut() {
                    if let Some(pack_value) = input_value.get_mut("pack") {
                        // Try to deserialize as old format
                        if let Ok(old_pack) =
                            serde_json::from_value::<OldPackOptions>(pack_value.clone())
                        {
                            // Convert to new format
                            let new_pack = old_pack.to_new_format();
                            // Serialize new pack to JSON value
                            *pack_value = serde_json::to_value(&new_pack).with_context(|| {
                                format!("Failed to convert pack options for input '{}'", input_name)
                            })?;
                            log::info!("Converted pack options for input: {}", input_name);
                        }
                    }
                }
            }
        }
    }

    Ok(old_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_old_to_new_conversion() {
        let old = OldPackOptions {
            enabled: true,
            max_size: (1024, 1024),
            power_of_two: false,
            padding: 4,
            extrude: 2,
            allow_trim: true,
            algorithm: PackAlgorithm::MaxRects,
            page_limit: Some(5),
            sort: PackSort::MaxSide,
            dedupe: true,
        };

        let new = old.to_new_format();

        assert!(new.enabled);
        assert_eq!(new.output.name, None);
        assert!(!new.output.overwrite);

        match new.mode {
            PackMode::Static(opts) => {
                assert_eq!(opts.max_size, (1024, 1024));
                assert!(!opts.power_of_two);
                assert_eq!(opts.padding, 4);
                assert_eq!(opts.extrude, 2);
                assert!(opts.allow_trim);
                assert_eq!(opts.page_limit, Some(5));
                assert!(opts.dedupe);
            }
            _ => panic!("Expected Static mode"),
        }
    }

    #[test]
    fn test_default_conversion() {
        let old = OldPackOptions::default();
        let new = old.to_new_format();

        assert!(!new.enabled);
        match new.mode {
            PackMode::Static(opts) => {
                assert_eq!(opts.max_size, (2048, 2048));
                assert!(opts.power_of_two);
                assert_eq!(opts.padding, 2);
                assert_eq!(opts.extrude, 1);
                assert!(!opts.allow_trim);
                assert_eq!(opts.page_limit, None);
                assert!(!opts.dedupe);
            }
            _ => panic!("Expected Static mode"),
        }
    }
}
