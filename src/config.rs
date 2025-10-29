use crate::glob::Glob;
use anyhow::Context;
use clap::ValueEnum;
use fs_err::tokio as fs;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema)]
#[schemars(description = "Asphalt configuration file")]
pub struct Config {
    #[schemars(description = "Roblox creator information (user or group)")]
    pub creator: Creator,

    #[serde(default)]
    #[schemars(description = "Code generation settings for asset references")]
    pub codegen: Codegen,

    #[schemars(description = "Asset input configurations mapped by name")]
    pub inputs: HashMap<String, Input>,
}

pub const CONFIG_FILES: &[&str] = &[
    "asphalt.json",
    "asphalt.json5",
    "asphalt.jsonc",
    "asphalt.toml",
];

impl Config {
    pub async fn read() -> anyhow::Result<Config> {
        // Try each config file in priority order
        for &file_name in CONFIG_FILES {
            if fs::metadata(file_name).await.is_ok() {
                let content = fs::read_to_string(file_name)
                    .await
                    .with_context(|| format!("Failed to read config file: {}", file_name))?;

                let config = match file_name {
                    name if name.ends_with(".json") => {
                        // Use fjson for lenient JSON parsing (supports trailing commas and comments)
                        let clean_json = fjson::to_json(&content).with_context(|| {
                            format!("Failed to parse JSON config file: {}", file_name)
                        })?;
                        serde_json::from_str::<Config>(&clean_json).with_context(|| {
                            format!("Failed to deserialize JSON config: {}", file_name)
                        })?
                    }
                    name if name.ends_with(".json5") => json5::from_str::<Config>(&content)
                        .with_context(|| {
                            format!("Failed to parse JSON5 config file: {}", file_name)
                        })?,
                    name if name.ends_with(".jsonc") => {
                        // Use fjson for JSONC files (supports comments and trailing commas)
                        let clean_json = fjson::to_json(&content).with_context(|| {
                            format!("Failed to parse JSONC config file: {}", file_name)
                        })?;
                        serde_json::from_str::<Config>(&clean_json).with_context(|| {
                            format!("Failed to deserialize JSONC config: {}", file_name)
                        })?
                    }
                    name if name.ends_with(".toml") => toml::from_str::<Config>(&content)
                        .with_context(|| {
                            format!("Failed to parse TOML config file: {}", file_name)
                        })?,
                    _ => {
                        return Err(anyhow::anyhow!(
                            "Unsupported config file format: {}",
                            file_name
                        ));
                    }
                };

                config
                    .validate()
                    .with_context(|| "Configuration validation failed")?;

                log::info!("Loaded configuration from {}", file_name);
                return Ok(config);
            }
        }

        Err(anyhow::anyhow!(
            "No configuration file found. Please create one of: {}",
            CONFIG_FILES.join(", ")
        ))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        for (input_name, input) in &self.inputs {
            if let Some(pack) = &input.pack {
                if let PackMode::Animated(opts) = &pack.mode {
                    if let Err(e) = Regex::new(&opts.frame_pattern) {
                        anyhow::bail!(
                            "Invalid animation frame regex in config (inputs.{}.pack.mode.animated.frame_pattern): '{}' — regex error: {}. Fix the pattern or disable animated packing for '{}'.",
                            input_name,
                            opts.frame_pattern,
                            e,
                            input_name
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

fn default_input_naming_convention() -> InputNamingConvention {
    InputNamingConvention::CamelCase
}

fn default_asset_naming_convention() -> AssetNamingConvention {
    AssetNamingConvention::Preserve
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, JsonSchema)]
#[serde(default)]
#[schemars(description = "Code generation settings")]
pub struct Codegen {
    #[schemars(
        description = "Code generation style: flat (file path-like) or nested (object property access)"
    )]
    pub style: CodegenStyle,
    #[schemars(description = "Generate TypeScript definition files (.d.ts) in addition to Luau")]
    pub typescript: bool,
    #[schemars(description = "Remove file extensions from generated asset paths")]
    pub strip_extensions: bool,
    #[schemars(description = "Generate Content objects instead of string asset IDs")]
    pub content: bool,
    #[serde(default = "default_input_naming_convention")]
    #[schemars(description = "Naming convention for input module names (default: camel_case)")]
    pub input_naming_convention: InputNamingConvention,
    #[serde(default = "default_asset_naming_convention")]
    #[schemars(
        description = "Naming convention for asset keys in generated code (default: preserve)"
    )]
    pub asset_naming_convention: AssetNamingConvention,
}

#[derive(Debug, Deserialize, Serialize, Clone, ValueEnum, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "Type of Roblox creator")]
pub enum CreatorType {
    User,
    Group,
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema)]
#[schemars(description = "Roblox creator information")]
pub struct Creator {
    #[serde(rename = "type")]
    #[schemars(description = "Creator type: user or group")]
    pub ty: CreatorType,
    #[schemars(description = "Creator ID (user ID or group ID)")]
    pub id: u64,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema)]
#[schemars(description = "Input asset configuration")]
pub struct Input {
    #[schemars(with = "String")]
    #[schemars(description = "Glob pattern to match asset files (e.g., 'assets/**/*.png')")]
    pub path: Glob,
    #[schemars(description = "Directory where generated code and packed assets will be written")]
    pub output_path: PathBuf,
    #[schemars(description = "Sprite packing/atlas generation configuration (optional)")]
    pub pack: Option<PackOptions>,
    #[serde(default = "default_true")]
    #[schemars(
        description = "Apply alpha bleeding to images to prevent edge artifacts (default: true)"
    )]
    pub bleed: bool,

    #[serde(default)]
    #[schemars(description = "Web assets that are already uploaded, mapped by path")]
    pub web: HashMap<String, WebAsset>,

    #[serde(default = "default_true")]
    #[schemars(description = "Warn for each duplicate file found (default: true)")]
    pub warn_each_duplicate: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema)]
#[schemars(description = "Web asset that has already been uploaded to Roblox")]
pub struct WebAsset {
    #[schemars(description = "Roblox asset ID of the uploaded asset")]
    pub id: u64,
}

fn default_pack_max_size() -> (u32, u32) {
    (2048, 2048)
}

fn default_pack_power_of_two() -> bool {
    true
}

fn default_pack_padding() -> u32 {
    2
}

fn default_pack_extrude() -> u32 {
    1
}

fn default_pack_algorithm() -> PackAlgorithm {
    PackAlgorithm::MaxRects
}

fn default_pack_sort() -> PackSort {
    PackSort::Area
}

fn default_frame_pattern() -> String {
    r"(?P<name>.+)_(\d+)".to_string()
}

fn default_min_frames() -> u32 {
    2
}

fn default_frame_duration_ms() -> u32 {
    100
}

fn default_loop() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema, Default)]
#[serde(default)]
#[schemars(description = "Output settings for generated atlases/spritesheets")]
pub struct OutputOptions {
    #[schemars(description = "Base name for output files (default: input name)")]
    pub name: Option<String>,
    #[schemars(description = "Overwrite existing outputs (default: false)")]
    pub overwrite: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "Animation layout for frame arrangement")]
pub enum AnimationLayout {
    #[default]
    #[schemars(description = "Arrange frames horizontally in a single row")]
    HorizontalStrip,
    #[schemars(description = "Arrange frames vertically in a single column")]
    VerticalStrip,
    #[schemars(description = "Arrange frames in a grid with optional column count")]
    Grid {
        #[schemars(description = "Number of columns (None = automatic)")]
        columns: Option<u32>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema)]
#[serde(default)]
#[schemars(description = "Static sprite packing options")]
pub struct StaticOptions {
    #[serde(default = "default_pack_max_size")]
    #[schemars(
        description = "Maximum atlas size as (width, height) in pixels (default: 2048x2048)"
    )]
    pub max_size: (u32, u32),
    #[serde(default = "default_pack_power_of_two")]
    #[schemars(description = "Constrain atlas dimensions to power-of-two sizes (default: true)")]
    pub power_of_two: bool,
    #[serde(default = "default_pack_padding")]
    #[schemars(description = "Padding between sprites in pixels (default: 2)")]
    pub padding: u32,
    #[serde(default = "default_pack_extrude")]
    #[schemars(
        description = "Pixels to extrude sprite edges for filtering artifacts (default: 1)"
    )]
    pub extrude: u32,
    #[schemars(description = "Allow trimming transparent borders from sprites (default: false)")]
    pub allow_trim: bool,
    #[serde(default = "default_pack_algorithm")]
    #[schemars(description = "Packing algorithm to use (default: max_rects)")]
    pub algorithm: PackAlgorithm,
    #[schemars(
        description = "Maximum number of atlas pages to generate (optional, unlimited by default)"
    )]
    pub page_limit: Option<u32>,
    #[serde(default = "default_pack_sort")]
    #[schemars(description = "Sprite sorting method for deterministic packing (default: area)")]
    pub sort: PackSort,
    #[schemars(description = "Enable deduplication of identical sprites (default: false)")]
    pub dedupe: bool,
}

impl Default for StaticOptions {
    fn default() -> Self {
        Self {
            max_size: default_pack_max_size(),
            power_of_two: default_pack_power_of_two(),
            padding: default_pack_padding(),
            extrude: default_pack_extrude(),
            allow_trim: false,
            algorithm: default_pack_algorithm(),
            page_limit: None,
            sort: default_pack_sort(),
            dedupe: false,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema)]
#[serde(default)]
#[schemars(description = "Animated spritesheet packing options")]
pub struct AnimatedOptions {
    #[serde(default = "default_frame_pattern")]
    #[schemars(
        description = "Regex pattern for detecting animation frames (default: '(?P<name>.+)_(\\d+)')"
    )]
    pub frame_pattern: String,
    #[serde(default = "default_min_frames")]
    #[schemars(
        description = "Minimum number of frames required to be considered an animation (default: 2)"
    )]
    pub min_frames: u32,
    #[serde(default)]
    #[schemars(description = "Layout for arranging animation frames (default: horizontal_strip)")]
    pub layout: AnimationLayout,
    #[serde(default = "default_frame_duration_ms")]
    #[schemars(description = "Default duration per frame in milliseconds (default: 100)")]
    pub default_frame_duration_ms: u32,
    #[serde(default = "default_loop")]
    #[schemars(description = "Whether animations should loop by default (default: true)")]
    pub default_loop: bool,
    #[serde(default = "default_pack_padding")]
    #[schemars(description = "Padding between frames in pixels (default: 2)")]
    pub padding: u32,
    #[serde(default = "default_pack_extrude")]
    #[schemars(description = "Pixels to extrude frame edges for filtering artifacts (default: 1)")]
    pub extrude: u32,
}

impl Default for AnimatedOptions {
    fn default() -> Self {
        Self {
            frame_pattern: default_frame_pattern(),
            min_frames: default_min_frames(),
            layout: AnimationLayout::default(),
            default_frame_duration_ms: default_frame_duration_ms(),
            default_loop: default_loop(),
            padding: default_pack_padding(),
            extrude: default_pack_extrude(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[schemars(description = "Packing mode: static or animated")]
pub enum PackMode {
    #[schemars(description = "Static sprite packing into atlases")]
    Static(StaticOptions),
    #[schemars(description = "Animated spritesheet packing")]
    Animated(AnimatedOptions),
}

impl Default for PackMode {
    fn default() -> Self {
        PackMode::Static(StaticOptions::default())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema, Default)]
#[serde(default)]
#[schemars(description = "Sprite packing configuration")]
pub struct PackOptions {
    #[schemars(description = "Enable sprite packing/atlas generation for this input")]
    pub enabled: bool,
    #[serde(default)]
    #[schemars(description = "Output settings for generated atlases/spritesheets")]
    pub output: OutputOptions,
    #[serde(flatten)]
    #[schemars(description = "Packing mode: static or animated")]
    pub mode: PackMode,
}

impl PackOptions {
    /// Get padding value from the current mode
    pub fn padding(&self) -> u32 {
        match &self.mode {
            PackMode::Static(opts) => opts.padding,
            PackMode::Animated(opts) => opts.padding,
        }
    }

    /// Get extrude value from the current mode
    pub fn extrude(&self) -> u32 {
        match &self.mode {
            PackMode::Static(opts) => opts.extrude,
            PackMode::Animated(opts) => opts.extrude,
        }
    }

    /// Get max_size (only available for Static mode)
    pub fn max_size(&self) -> (u32, u32) {
        match &self.mode {
            PackMode::Static(opts) => opts.max_size,
            PackMode::Animated(_) => (2048, 2048), // Default for animated
        }
    }

    /// Get power_of_two (only available for Static mode)
    pub fn power_of_two(&self) -> bool {
        match &self.mode {
            PackMode::Static(opts) => opts.power_of_two,
            PackMode::Animated(_) => false, // Not used for animated
        }
    }

    /// Get page_limit (only available for Static mode)
    pub fn page_limit(&self) -> Option<u32> {
        match &self.mode {
            PackMode::Static(opts) => opts.page_limit,
            PackMode::Animated(_) => None, // Not used for animated
        }
    }

    /// Get sort (only available for Static mode)
    pub fn sort(&self) -> &PackSort {
        match &self.mode {
            PackMode::Static(opts) => &opts.sort,
            PackMode::Animated(_) => &PackSort::Area, // Default for animated
        }
    }

    /// Get dedupe (only available for Static mode)
    pub fn dedupe(&self) -> bool {
        match &self.mode {
            PackMode::Static(opts) => opts.dedupe,
            PackMode::Animated(_) => false, // Not used for animated
        }
    }

    /// Get allow_trim (only available for Static mode)
    pub fn allow_trim(&self) -> bool {
        match &self.mode {
            PackMode::Static(opts) => opts.allow_trim,
            PackMode::Animated(_) => false, // Not used for animated
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, ValueEnum, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "Packing algorithm to use")]
pub enum PackAlgorithm {
    MaxRects,
    Guillotine,
}

#[derive(Debug, Deserialize, Serialize, Clone, ValueEnum, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "Sprite sorting method for deterministic packing")]
pub enum PackSort {
    Area,
    MaxSide,
    Name,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "Code generation style")]
pub enum CodegenStyle {
    #[default]
    Flat,
    Nested,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "Naming convention for input module names")]
#[allow(clippy::enum_variant_names)]
pub enum InputNamingConvention {
    #[schemars(description = "lowercase_with_underscores (e.g., 'my_input')")]
    SnakeCase,
    #[default]
    #[schemars(description = "firstWordLowerRestCapitalized (e.g., 'myInput') - default")]
    CamelCase,
    #[schemars(description = "AllWordsCapitalized (e.g., 'MyInput')")]
    PascalCase,
    #[schemars(description = "UPPERCASE_WITH_UNDERSCORES (e.g., 'MY_INPUT')")]
    ScreamingSnakeCase,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "Naming convention for asset keys in generated code")]
#[allow(clippy::enum_variant_names)]
pub enum AssetNamingConvention {
    #[schemars(description = "lowercase_with_underscores (e.g., 'my_asset_name')")]
    SnakeCase,
    #[schemars(description = "firstWordLowerRestCapitalized (e.g., 'myAssetName')")]
    CamelCase,
    #[schemars(description = "AllWordsCapitalized (e.g., 'MyAssetName')")]
    PascalCase,
    #[schemars(description = "UPPERCASE_WITH_UNDERSCORES (e.g., 'MY_ASSET_NAME')")]
    ScreamingSnakeCase,
    #[schemars(description = "lowercase-with-hyphens (e.g., 'my-asset-name')")]
    KebabCase,
    #[default]
    #[schemars(
        description = "Preserve original name, quote if contains special characters - default"
    )]
    Preserve,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_options_static_mode_deserialization() {
        let json = r#"{
            "enabled": true,
            "type": "static",
            "max_size": [1024, 1024],
            "power_of_two": false,
            "padding": 4,
            "extrude": 2,
            "allow_trim": true,
            "algorithm": "max_rects",
            "page_limit": 5,
            "sort": "max_side",
            "dedupe": true
        }"#;

        let opts: PackOptions = serde_json::from_str(json).expect("Failed to deserialize");
        assert!(opts.enabled);
        match opts.mode {
            PackMode::Static(static_opts) => {
                assert_eq!(static_opts.max_size, (1024, 1024));
                assert!(!static_opts.power_of_two);
                assert_eq!(static_opts.padding, 4);
                assert_eq!(static_opts.extrude, 2);
                assert!(static_opts.allow_trim);
                assert_eq!(static_opts.page_limit, Some(5));
                assert!(static_opts.dedupe);
            }
            _ => panic!("Expected Static mode"),
        }
    }

    #[test]
    fn test_pack_options_animated_mode_deserialization() {
        let json = r#"{
            "enabled": true,
            "type": "animated",
            "frame_pattern": "(?P<name>.+)_(\\d+)",
            "min_frames": 3,
            "layout": "horizontal_strip",
            "default_frame_duration_ms": 50,
            "default_loop": false,
            "padding": 1,
            "extrude": 0
        }"#;

        let opts: PackOptions = serde_json::from_str(json).expect("Failed to deserialize");
        assert!(opts.enabled);
        match opts.mode {
            PackMode::Animated(anim_opts) => {
                assert_eq!(anim_opts.frame_pattern, r"(?P<name>.+)_(\d+)");
                assert_eq!(anim_opts.min_frames, 3);
                assert_eq!(anim_opts.default_frame_duration_ms, 50);
                assert!(!anim_opts.default_loop);
                assert_eq!(anim_opts.padding, 1);
                assert_eq!(anim_opts.extrude, 0);
            }
            _ => panic!("Expected Animated mode"),
        }
    }

    #[test]
    fn test_pack_options_defaults() {
        let json = r#"{
            "enabled": false,
            "type": "static"
        }"#;

        let opts: PackOptions = serde_json::from_str(json).expect("Failed to deserialize");
        assert!(!opts.enabled);
        // Should default to Static mode
        assert!(matches!(opts.mode, PackMode::Static(_)));
    }

    #[test]
    fn test_pack_mode_switching() {
        // Test that we can deserialize both modes independently
        let static_json = r#"{"type": "static"}"#;
        let static_mode: PackMode =
            serde_json::from_str(static_json).expect("Failed to parse static");
        assert!(matches!(static_mode, PackMode::Static(_)));

        let animated_json = r#"{"type": "animated"}"#;
        let animated_mode: PackMode =
            serde_json::from_str(animated_json).expect("Failed to parse animated");
        assert!(matches!(animated_mode, PackMode::Animated(_)));
    }

    #[test]
    fn test_invalid_pack_mode() {
        let json = r#"{"type": "invalid_mode"}"#;
        let result: Result<PackMode, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_animation_layout_grid() {
        let json = r#"{"type": "animated", "layout": {"grid": {"columns": 4}}}"#;
        let mode: PackMode = serde_json::from_str(json).expect("Failed to parse");
        match mode {
            PackMode::Animated(opts) => match opts.layout {
                AnimationLayout::Grid { columns } => {
                    assert_eq!(columns, Some(4));
                }
                _ => panic!("Expected Grid layout"),
            },
            _ => panic!("Expected Animated mode"),
        }
    }

    #[test]
    fn test_config_validate_invalid_frame_pattern() {
        let toml_config = r#"
[creator]
type = "user"
id = 123

[codegen]
typescript = true

[inputs.assets]
path = "assets/**/*"
output_path = "src/shared"

[inputs.assets.pack]
enabled = true
type = "animated"
frame_pattern = "[unclosed"
"#;

        let config: Config = toml::from_str(toml_config).expect("Failed to parse test config");
        let err = config.validate().unwrap_err();
        let err_msg = err.to_string();

        assert!(err_msg.contains("inputs.assets.pack.mode.animated.frame_pattern"));
        assert!(err_msg.contains("'[unclosed'"));
        assert!(err_msg.contains("regex error"));
        assert!(err_msg.contains("unclosed character class"));
        assert!(err_msg.contains("Fix the pattern or disable animated packing for 'assets'"));
    }

    #[test]
    fn test_config_validate_valid_default_pattern() {
        let toml_config = r#"
[creator]
type = "user"
id = 123

[codegen]
typescript = true

[inputs.assets]
path = "assets/**/*"
output_path = "src/shared"

[inputs.assets.pack]
enabled = true
type = "animated"
"#;

        let config: Config = toml::from_str(toml_config).expect("Failed to parse test config");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validate_no_animated_inputs() {
        let toml_config = r#"
[creator]
type = "user"
id = 123

[codegen]
typescript = true

[inputs.assets]
path = "assets/**/*"
output_path = "src/shared"
"#;

        let config: Config = toml::from_str(toml_config).expect("Failed to parse test config");
        assert!(config.validate().is_ok());
    }
}
