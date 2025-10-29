use crate::{
    asset::Asset,
    config::{PackMode, PackOptions, PackSort},
};
use anyhow::{Context, Result, bail};
use image::RgbaImage;
use regex::Regex;
use std::collections::HashMap;

pub mod algorithm;
pub mod manifest;
pub mod rect;

pub use manifest::{AtlasManifest, SpriteInfo};
pub use rect::{Rect, Size};

/// A sprite to be packed into an atlas
#[derive(Debug, Clone)]
pub struct Sprite {
    pub name: String,
    pub data: Vec<u8>,
    pub size: Size,
    #[allow(dead_code)]
    pub hash: String,
}

/// An animation strip that has been combined from multiple frames
#[derive(Debug, Clone)]
pub struct AnimationStrip {
    /// The combined strip image data
    pub strip_sprite: Sprite,
    /// Number of frames in the animation
    pub frame_count: u32,
    /// Size of a single frame
    pub frame_size: Size,
    /// Layout used for the strip
    pub layout: crate::config::AnimationLayout,
    /// Duration of each frame in milliseconds
    pub frame_duration_ms: u32,
    /// Whether the animation should loop
    pub loops: bool,
}

/// Item that can be packed - either a static sprite or an animation strip
#[derive(Debug, Clone)]
pub enum PackableItem {
    Static(Sprite),
    Animated(AnimationStrip),
}

impl PackableItem {
    /// Get the sprite data (either the static sprite or the animation strip)
    pub fn sprite(&self) -> &Sprite {
        match self {
            PackableItem::Static(sprite) => sprite,
            PackableItem::Animated(anim) => &anim.strip_sprite,
        }
    }

    /// Get a mutable reference to the sprite data
    pub fn sprite_mut(&mut self) -> &mut Sprite {
        match self {
            PackableItem::Static(sprite) => sprite,
            PackableItem::Animated(anim) => &mut anim.strip_sprite,
        }
    }
}

/// Result of packing sprites into atlases
#[derive(Debug)]
pub struct PackResult {
    pub atlases: Vec<Atlas>,
    pub manifest: AtlasManifest,
}

/// A single atlas page containing packed sprites
#[derive(Debug)]
pub struct Atlas {
    pub page_index: usize,
    pub image_data: Vec<u8>,
    #[allow(dead_code)]
    pub size: Size,
    pub sprites: Vec<PackedSprite>,
}

/// A sprite that has been placed in an atlas
#[derive(Debug, Clone)]
pub struct PackedSprite {
    pub item: PackableItem,
    pub rect: Rect,
    pub trimmed: bool,
    pub sprite_source_size: Option<Rect>,
}

/// Main packing orchestrator
pub struct Packer {
    options: PackOptions,
}

impl Packer {
    pub fn new(options: PackOptions) -> Self {
        Self { options }
    }

    /// Pack a collection of assets into atlases
    pub fn pack_assets(&self, assets: &[Asset], input_name: &str) -> Result<PackResult> {
        if !self.options.enabled {
            bail!("Packing is not enabled for input '{}'", input_name);
        }

        // Convert assets to sprites
        let sprites = self.assets_to_sprites(assets)?;

        if sprites.is_empty() {
            return Ok(PackResult {
                atlases: Vec::new(),
                manifest: AtlasManifest::new(input_name.to_string()),
            });
        }

        // Sort sprites for deterministic packing
        let mut sorted_sprites = sprites;
        self.sort_sprites(&mut sorted_sprites);

        // Validate sprite sizes
        self.validate_sprite_sizes(&sorted_sprites)?;

        // Pack sprites into pages
        let atlases = self.pack_sprites_to_atlases(sorted_sprites)?;

        // Check page limit
        if let Some(limit) = self.options.page_limit()
            && atlases.len() > limit as usize
        {
            bail!(
                "Packing would require {} pages but limit is {}. Consider increasing max_size or page_limit.",
                atlases.len(),
                limit
            );
        }

        // Generate manifest
        let manifest = self.create_manifest(&atlases, input_name)?;

        Ok(PackResult { atlases, manifest })
    }

    fn assets_to_sprites(&self, assets: &[Asset]) -> Result<Vec<PackableItem>> {
        // Check if we're in Animated mode - if so, use animation detection
        if let PackMode::Animated(anim_opts) = &self.options.mode {
            return self.detect_animations(assets, anim_opts);
        }

        // Static mode - just convert assets to sprites normally
        let mut items = Vec::new();
        let mut seen_hashes = HashMap::new();

        for asset in assets {
            // Only pack image assets
            if !matches!(asset.ty, crate::asset::AssetType::Image(_)) {
                continue;
            }

            // Load image to get dimensions
            let image = image::load_from_memory(&asset.data)
                .with_context(|| format!("Failed to load image: {}", asset.path.display()))?;

            let size = Size {
                width: image.width(),
                height: image.height(),
            };

            let name = asset
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            // Handle deduplication
            if self.options.dedupe() {
                if let Some(existing_name) = seen_hashes.get(&asset.hash) {
                    log::debug!(
                        "Skipping duplicate sprite '{}' (same as '{}')",
                        name,
                        existing_name
                    );
                    continue;
                }
                seen_hashes.insert(asset.hash.clone(), name.clone());
            }

            items.push(PackableItem::Static(Sprite {
                name,
                data: asset.data.clone(),
                size,
                hash: asset.hash.clone(),
            }));
        }

        Ok(items)
    }

    fn detect_animations(
        &self,
        assets: &[Asset],
        anim_opts: &crate::config::AnimatedOptions,
    ) -> Result<Vec<PackableItem>> {
        use std::collections::BTreeMap;

        // Compile regex pattern
        let frame_regex = Regex::new(&anim_opts.frame_pattern)
            .with_context(|| format!("Invalid frame_pattern regex: {}", anim_opts.frame_pattern))?;

        // Group sprites by animation name
        let mut animation_groups: HashMap<String, BTreeMap<u32, Sprite>> = HashMap::new();
        let mut static_sprites = Vec::new();

        for asset in assets {
            // Only pack image assets
            if !matches!(asset.ty, crate::asset::AssetType::Image(_)) {
                continue;
            }

            // Load image to get dimensions
            let image = image::load_from_memory(&asset.data)
                .with_context(|| format!("Failed to load image: {}", asset.path.display()))?;

            let size = Size {
                width: image.width(),
                height: image.height(),
            };

            let filename = asset
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");

            // Try to match against animation pattern
            if let Some(captures) = frame_regex.captures(filename) {
                // Extract animation name (named group 'name')
                let anim_name = captures
                    .name("name")
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_else(|| filename.to_string());

                // Extract frame number (second capture group, should be digits)
                let frame_num = if let Some(num_match) = captures.get(2) {
                    num_match.as_str().parse::<u32>().unwrap_or(0)
                } else {
                    log::warn!("Failed to extract frame number from '{}'", filename);
                    0
                };

                log::debug!(
                    "Detected animation frame: '{}' frame {}",
                    anim_name,
                    frame_num
                );

                let sprite = Sprite {
                    name: filename.to_string(),
                    data: asset.data.clone(),
                    size,
                    hash: asset.hash.clone(),
                };

                animation_groups
                    .entry(anim_name)
                    .or_default()
                    .insert(frame_num, sprite);
            } else {
                // Doesn't match animation pattern - treat as static sprite
                log::debug!("Sprite '{}' doesn't match animation pattern", filename);

                static_sprites.push(Sprite {
                    name: filename.to_string(),
                    data: asset.data.clone(),
                    size,
                    hash: asset.hash.clone(),
                });
            }
        }

        // Process animation groups - combine frames or split to static if not enough frames
        let mut final_items: Vec<PackableItem> = static_sprites
            .into_iter()
            .map(PackableItem::Static)
            .collect();

        for (anim_name, frames) in animation_groups {
            let frame_count = frames.len() as u32;

            if frame_count < anim_opts.min_frames {
                log::info!(
                    "Animation '{}' has only {} frames (min: {}), treating frames as static sprites",
                    anim_name,
                    frame_count,
                    anim_opts.min_frames
                );

                // Add all frames as individual static sprites
                for (_frame_num, sprite) in frames {
                    final_items.push(PackableItem::Static(sprite));
                }
            } else {
                log::info!(
                    "Detected animation '{}' with {} frames",
                    anim_name,
                    frame_count
                );

                // Convert frames to ordered Vec
                let ordered_frames: Vec<Sprite> = frames.into_values().collect();

                // Combine frames into a single animation strip
                let animation_strip = self.combine_frames_into_strip(
                    &anim_name,
                    ordered_frames,
                    &anim_opts.layout,
                    anim_opts.default_frame_duration_ms,
                    anim_opts.default_loop,
                )?;
                final_items.push(PackableItem::Animated(animation_strip));
            }
        }

        Ok(final_items)
    }

    /// Combine animation frames into a single strip sprite
    fn combine_frames_into_strip(
        &self,
        anim_name: &str,
        frames: Vec<Sprite>,
        layout: &crate::config::AnimationLayout,
        frame_duration_ms: u32,
        loops: bool,
    ) -> Result<AnimationStrip> {
        use crate::config::AnimationLayout;
        use image::{ImageBuffer, RgbaImage};
        use std::io::Cursor;

        if frames.is_empty() {
            bail!("Cannot combine empty animation frames for '{}'", anim_name);
        }

        // All frames should have the same size - use the first frame as reference
        let frame_width = frames[0].size.width;
        let frame_height = frames[0].size.height;
        let frame_count = frames.len() as u32;

        // Validate all frames have the same size
        for (i, frame) in frames.iter().enumerate() {
            if frame.size.width != frame_width || frame.size.height != frame_height {
                log::warn!(
                    "Animation '{}' frame {} has different size ({}x{}) than first frame ({}x{})",
                    anim_name,
                    i,
                    frame.size.width,
                    frame.size.height,
                    frame_width,
                    frame_height
                );
            }
        }

        // Calculate strip dimensions based on layout
        let (strip_width, strip_height, columns) = match layout {
            AnimationLayout::HorizontalStrip => {
                (frame_width * frame_count, frame_height, frame_count)
            }
            AnimationLayout::VerticalStrip => (frame_width, frame_height * frame_count, 1),
            AnimationLayout::Grid { columns } => {
                let cols = columns.unwrap_or_else(|| {
                    // Auto-calculate columns - try to make it roughly square
                    (frame_count as f32).sqrt().ceil() as u32
                });
                let rows = frame_count.div_ceil(cols);
                (frame_width * cols, frame_height * rows, cols)
            }
        };

        log::debug!(
            "Creating {} strip for animation '{}': {}x{} ({} frames, {} columns)",
            match layout {
                AnimationLayout::HorizontalStrip => "horizontal",
                AnimationLayout::VerticalStrip => "vertical",
                AnimationLayout::Grid { .. } => "grid",
            },
            anim_name,
            strip_width,
            strip_height,
            frame_count,
            columns
        );

        // Create blank strip image
        let mut strip_image: RgbaImage = ImageBuffer::new(strip_width, strip_height);

        // Copy each frame into the strip
        for (i, frame) in frames.iter().enumerate() {
            let frame_idx = i as u32;

            // Calculate position based on layout
            let (x_offset, y_offset) = match layout {
                AnimationLayout::HorizontalStrip => (frame_idx * frame_width, 0),
                AnimationLayout::VerticalStrip => (0, frame_idx * frame_height),
                AnimationLayout::Grid { .. } => {
                    let col = frame_idx % columns;
                    let row = frame_idx / columns;
                    (col * frame_width, row * frame_height)
                }
            };

            // Load frame image
            let frame_image = image::load_from_memory(&frame.data).with_context(|| {
                format!("Failed to load frame {} for animation '{}'", i, anim_name)
            })?;
            let frame_rgba = frame_image.to_rgba8();

            // Copy pixels from frame to strip
            for y in 0..frame_rgba.height() {
                for x in 0..frame_rgba.width() {
                    if let Some(pixel) = frame_rgba.get_pixel_checked(x, y) {
                        if x_offset + x < strip_width && y_offset + y < strip_height {
                            strip_image.put_pixel(x_offset + x, y_offset + y, *pixel);
                        }
                    }
                }
            }
        }

        // Encode strip as PNG
        let mut buffer = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(strip_image)
            .write_to(&mut buffer, image::ImageFormat::Png)
            .with_context(|| format!("Failed to encode strip for animation '{}'", anim_name))?;

        // Calculate a hash for the combined strip (use first frame's hash as base)
        let strip_hash = frames[0].hash.clone();

        let strip_sprite = Sprite {
            name: anim_name.to_string(),
            data: buffer.into_inner(),
            size: Size {
                width: strip_width,
                height: strip_height,
            },
            hash: strip_hash,
        };

        Ok(AnimationStrip {
            strip_sprite,
            frame_count,
            frame_size: Size {
                width: frame_width,
                height: frame_height,
            },
            layout: layout.clone(),
            frame_duration_ms,
            loops,
        })
    }

    fn sort_sprites(&self, items: &mut [PackableItem]) {
        items.sort_by(|a, b| {
            let sprite_a = a.sprite();
            let sprite_b = b.sprite();

            let primary_cmp = match self.options.sort() {
                PackSort::Area => {
                    let area_a = sprite_a.size.width * sprite_a.size.height;
                    let area_b = sprite_b.size.width * sprite_b.size.height;
                    area_b.cmp(&area_a) // Descending order (largest first)
                }
                PackSort::MaxSide => {
                    let max_a = sprite_a.size.width.max(sprite_a.size.height);
                    let max_b = sprite_b.size.width.max(sprite_b.size.height);
                    max_b.cmp(&max_a) // Descending order (largest first)
                }
                PackSort::Name => sprite_a.name.cmp(&sprite_b.name),
            };

            // Use name as tie-breaker for deterministic results
            primary_cmp.then_with(|| sprite_a.name.cmp(&sprite_b.name))
        });
    }

    fn validate_sprite_sizes(&self, items: &[PackableItem]) -> Result<()> {
        let (max_width, max_height) = self.options.max_size();

        for item in items {
            let sprite = item.sprite();
            if sprite.size.width > max_width || sprite.size.height > max_height {
                bail!(
                    "Sprite '{}' ({}x{}) exceeds maximum atlas size ({}x{}). Consider increasing max_size or excluding this sprite from packing.",
                    sprite.name,
                    sprite.size.width,
                    sprite.size.height,
                    max_width,
                    max_height
                );
            }
        }

        Ok(())
    }

    fn pack_sprites_to_atlases(&self, items: Vec<PackableItem>) -> Result<Vec<Atlas>> {
        let mut atlases = Vec::new();
        let mut remaining_items = items;

        while !remaining_items.is_empty() {
            let page_index = atlases.len();
            let (atlas, unpacked_items) = self.pack_single_atlas(remaining_items, page_index)?;
            atlases.push(atlas);
            remaining_items = unpacked_items;
        }

        Ok(atlases)
    }

    fn pack_single_atlas(
        &self,
        items: Vec<PackableItem>,
        page_index: usize,
    ) -> Result<(Atlas, Vec<PackableItem>)> {
        use algorithm::MaxRectsPacker;

        let atlas_size = if self.options.power_of_two() {
            // Find the next power of two that fits our max size
            let (max_width, max_height) = self.options.max_size();
            let width = max_width.next_power_of_two();
            let height = max_height.next_power_of_two();
            Size { width, height }
        } else {
            let (max_width, max_height) = self.options.max_size();
            Size {
                width: max_width,
                height: max_height,
            }
        };

        let mut packer = MaxRectsPacker::new(atlas_size);
        let mut packed_sprites = Vec::new();
        let mut unpacked_items = Vec::new();

        for mut item in items {
            // Trim sprite to remove transparent borders
            let original_rect = self.trim_sprite(item.sprite_mut());

            let sprite = item.sprite();
            // Account for padding in placement
            let padding = self.options.padding();
            let required_size = Size {
                width: sprite.size.width + 2 * padding,
                height: sprite.size.height + 2 * padding,
            };

            if let Some(rect) = packer.pack(required_size) {
                // Adjust rect to account for padding
                let sprite_rect = Rect {
                    x: rect.x + padding,
                    y: rect.y + padding,
                    width: sprite.size.width,
                    height: sprite.size.height,
                };

                packed_sprites.push(PackedSprite {
                    item,
                    rect: sprite_rect,
                    trimmed: original_rect.is_some(),
                    sprite_source_size: original_rect,
                });
            } else {
                unpacked_items.push(item);
            }
        }

        // Create atlas image
        let image_data = self.render_atlas(&packed_sprites, atlas_size)?;

        Ok((
            Atlas {
                page_index,
                image_data,
                size: atlas_size,
                sprites: packed_sprites,
            },
            unpacked_items,
        ))
    }

    fn trim_sprite(&self, sprite: &mut Sprite) -> Option<Rect> {
        if !self.options.allow_trim() {
            return None;
        }
        use std::io::Cursor;

        let img = image::load_from_memory(&sprite.data).ok()?;
        let rgba = img.to_rgba8();
        let width = rgba.width() as usize;
        let height = rgba.height() as usize;

        if width == 0 || height == 0 {
            return None;
        }

        let pixels = rgba.as_raw();

        // Find bounding box of non-transparent pixels
        let mut min_x = width;
        let mut max_x = 0;
        let mut min_y = height;
        let mut max_y = 0;

        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) * 4;
                if pixels[idx + 3] != 0 {
                    if x < min_x {
                        min_x = x;
                    }
                    if x > max_x {
                        max_x = x;
                    }
                    if y < min_y {
                        min_y = y;
                    }
                    if y > max_y {
                        max_y = y;
                    }
                }
            }
        }

        if min_x > max_x || min_y > max_y {
            return None; // No opaque pixels
        }

        let trimmed_width = max_x - min_x + 1;
        let trimmed_height = max_y - min_y + 1;

        if trimmed_width == width && trimmed_height == height {
            return None; // No trimming needed
        }

        // Crop the image
        let sub_img = image::imageops::crop_imm(
            &rgba,
            min_x as u32,
            min_y as u32,
            trimmed_width as u32,
            trimmed_height as u32,
        );
        let cropped = sub_img.to_image();

        // Encode back to PNG
        let mut buffer = Cursor::new(Vec::new());
        cropped
            .write_to(&mut buffer, image::ImageFormat::Png)
            .ok()?;

        let original_size = sprite.size;
        sprite.data = buffer.into_inner();
        sprite.size = Size {
            width: trimmed_width as u32,
            height: trimmed_height as u32,
        };

        Some(Rect {
            x: 0,
            y: 0,
            width: original_size.width,
            height: original_size.height,
        })
    }

    fn render_atlas(&self, packed_sprites: &[PackedSprite], atlas_size: Size) -> Result<Vec<u8>> {
        use image::{DynamicImage, ImageBuffer, RgbaImage};
        use std::io::Cursor;

        let mut atlas_image: RgbaImage = ImageBuffer::new(atlas_size.width, atlas_size.height);

        log::debug!(
            "Rendering atlas {}x{} with {} sprites",
            atlas_size.width,
            atlas_size.height,
            packed_sprites.len()
        );

        for (i, packed_sprite) in packed_sprites.iter().enumerate() {
            let sprite = packed_sprite.item.sprite();
            log::debug!(
                "Rendering sprite {} '{}' at ({}, {}) size {}x{}",
                i,
                sprite.name,
                packed_sprite.rect.x,
                packed_sprite.rect.y,
                packed_sprite.rect.width,
                packed_sprite.rect.height
            );

            let sprite_image = image::load_from_memory(&sprite.data)?;
            let sprite_rgba = sprite_image.to_rgba8();

            log::debug!(
                "Loaded sprite image {}x{}",
                sprite_rgba.width(),
                sprite_rgba.height()
            );

            // Copy sprite to atlas at the correct position
            for y in 0..packed_sprite.rect.height {
                for x in 0..packed_sprite.rect.width {
                    if let Some(sprite_pixel) = sprite_rgba.get_pixel_checked(x, y) {
                        atlas_image.put_pixel(
                            packed_sprite.rect.x + x,
                            packed_sprite.rect.y + y,
                            *sprite_pixel,
                        );
                    }
                }
            }

            // Apply extrude if configured
            if self.options.extrude() > 0 {
                self.apply_extrude(&mut atlas_image, packed_sprite)?;
            }

            log::debug!("Finished rendering sprite '{}'", sprite.name);
        }

        log::debug!("Applying alpha bleeding to atlas image");
        let mut atlas_dynamic = DynamicImage::ImageRgba8(atlas_image);
        crate::util::alpha_bleed::alpha_bleed(&mut atlas_dynamic);

        // Encode as PNG
        let mut buffer = Cursor::new(Vec::new());
        atlas_dynamic.write_to(&mut buffer, image::ImageFormat::Png)?;
        Ok(buffer.into_inner())
    }

    fn apply_extrude(
        &self,
        atlas_image: &mut RgbaImage,
        packed_sprite: &PackedSprite,
    ) -> Result<()> {
        let extrude = self.options.extrude();
        let rect = &packed_sprite.rect;

        for e in 1..=extrude {
            let e = e as i32;

            for y in 0..rect.height {
                if rect.x >= e as u32 {
                    let edge_pixel = atlas_image.get_pixel(rect.x, rect.y + y);
                    atlas_image.put_pixel(rect.x - e as u32, rect.y + y, *edge_pixel);
                }

                if rect.x + rect.width + (e as u32) <= atlas_image.width() {
                    let edge_pixel = atlas_image.get_pixel(rect.x + rect.width - 1, rect.y + y);
                    atlas_image.put_pixel(
                        rect.x + rect.width + e as u32 - 1,
                        rect.y + y,
                        *edge_pixel,
                    );
                }
            }

            for x in 0..rect.width {
                if rect.y >= e as u32 {
                    let edge_pixel = atlas_image.get_pixel(rect.x + x, rect.y);
                    atlas_image.put_pixel(rect.x + x, rect.y - e as u32, *edge_pixel);
                }

                if rect.y + rect.height + (e as u32) <= atlas_image.height() {
                    let edge_pixel = atlas_image.get_pixel(rect.x + x, rect.y + rect.height - 1);
                    atlas_image.put_pixel(
                        rect.x + x,
                        rect.y + rect.height + e as u32 - 1,
                        *edge_pixel,
                    );
                }
            }
        }

        Ok(())
    }

    fn create_manifest(&self, atlases: &[Atlas], input_name: &str) -> Result<AtlasManifest> {
        use crate::pack::manifest::{AnimationInfo, AnimationLayoutInfo};

        let mut manifest = AtlasManifest::new(input_name.to_string());

        for atlas in atlases {
            for packed_sprite in &atlas.sprites {
                let sprite = packed_sprite.item.sprite();

                // Extract animation metadata if this is an animated item
                let animation = match &packed_sprite.item {
                    PackableItem::Animated(anim) => {
                        let layout_info = match &anim.layout {
                            crate::config::AnimationLayout::HorizontalStrip => {
                                AnimationLayoutInfo::HorizontalStrip
                            }
                            crate::config::AnimationLayout::VerticalStrip => {
                                AnimationLayoutInfo::VerticalStrip
                            }
                            crate::config::AnimationLayout::Grid { columns } => {
                                AnimationLayoutInfo::Grid {
                                    columns: columns.unwrap_or(4),
                                }
                            }
                        };

                        Some(AnimationInfo {
                            frame_count: anim.frame_count,
                            frame_size: anim.frame_size,
                            layout: layout_info,
                            frame_duration_ms: anim.frame_duration_ms,
                            loops: anim.loops,
                        })
                    }
                    PackableItem::Static(_) => None,
                };

                let sprite_info = SpriteInfo {
                    name: sprite.name.clone(),
                    rect: packed_sprite.rect,
                    source_size: sprite.size,
                    trimmed: packed_sprite.trimmed,
                    sprite_source_size: packed_sprite.sprite_source_size,
                    page_index: atlas.page_index,
                    animation,
                };
                manifest.add_sprite(sprite_info);
            }
        }

        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AnimatedOptions, AnimationLayout, OutputOptions, PackMode, StaticOptions};

    #[test]
    fn test_packable_item_accessors() {
        let static_sprite = Sprite {
            name: "test".to_string(),
            data: vec![],
            size: Size {
                width: 64,
                height: 64,
            },
            hash: "hash123".to_string(),
        };

        let item = PackableItem::Static(static_sprite.clone());
        assert_eq!(item.sprite().name, "test");

        let anim_strip = AnimationStrip {
            strip_sprite: static_sprite.clone(),
            frame_count: 4,
            frame_size: Size {
                width: 16,
                height: 16,
            },
            layout: AnimationLayout::HorizontalStrip,
            frame_duration_ms: 100,
            loops: true,
        };

        let anim_item = PackableItem::Animated(anim_strip);
        assert_eq!(anim_item.sprite().name, "test");
    }

    #[test]
    fn test_animation_detection_horizontal_strip() {
        // Create a simple 2x2 red PNG image for testing
        let test_image_data = create_test_image_png(2, 2, [255, 0, 0, 255]);

        let assets = vec![
            create_test_asset("walk_001.png", test_image_data.clone(), "hash1"),
            create_test_asset("walk_002.png", test_image_data.clone(), "hash2"),
            create_test_asset("walk_003.png", test_image_data.clone(), "hash3"),
        ];

        let animated_options = AnimatedOptions {
            frame_pattern: r"^(?P<name>.+?)_(\d+)$".to_string(),
            min_frames: 2,
            layout: AnimationLayout::HorizontalStrip,
            default_frame_duration_ms: 100,
            default_loop: true,
            ..Default::default()
        };

        let options = PackOptions {
            enabled: true,
            mode: PackMode::Animated(animated_options),
            output: OutputOptions::default(),
        };

        let packer = Packer::new(options);
        let result = packer.detect_animations(
            &assets,
            &match &packer.options.mode {
                PackMode::Animated(opts) => opts.clone(),
                _ => panic!("Expected Animated mode"),
            },
        );

        assert!(result.is_ok());
        let items = result.unwrap();
        assert_eq!(items.len(), 1); // Should be one animation

        match &items[0] {
            PackableItem::Animated(anim) => {
                assert_eq!(anim.frame_count, 3);
                assert_eq!(anim.frame_duration_ms, 100);
                assert!(anim.loops);
                assert_eq!(anim.strip_sprite.size.width, 6); // 3 frames * 2px width
                assert_eq!(anim.strip_sprite.size.height, 2);
            }
            _ => panic!("Expected animated item"),
        }
    }

    #[test]
    fn test_animation_detection_min_frames() {
        let test_image_data = create_test_image_png(2, 2, [255, 0, 0, 255]);

        // Only one frame - should be treated as static
        let assets = vec![create_test_asset(
            "walk_001.png",
            test_image_data.clone(),
            "hash1",
        )];

        let animated_options = AnimatedOptions {
            frame_pattern: r"^(?P<name>.+?)_(\d+)$".to_string(),
            min_frames: 2,
            layout: AnimationLayout::HorizontalStrip,
            default_frame_duration_ms: 100,
            default_loop: true,
            ..Default::default()
        };

        let options = PackOptions {
            enabled: true,
            mode: PackMode::Animated(animated_options),
            output: OutputOptions::default(),
        };

        let packer = Packer::new(options);
        let result = packer.detect_animations(
            &assets,
            &match &packer.options.mode {
                PackMode::Animated(opts) => opts.clone(),
                _ => panic!("Expected Animated mode"),
            },
        );

        assert!(result.is_ok());
        let items = result.unwrap();
        assert_eq!(items.len(), 1);

        // Should be static, not animated
        match &items[0] {
            PackableItem::Static(sprite) => {
                assert_eq!(sprite.name, "walk_001");
            }
            _ => panic!("Expected static item when min_frames not met"),
        }
    }

    #[test]
    fn test_animation_detection_vertical_strip() {
        let test_image_data = create_test_image_png(2, 2, [0, 255, 0, 255]);

        let assets = vec![
            create_test_asset("idle_01.png", test_image_data.clone(), "hash1"),
            create_test_asset("idle_02.png", test_image_data.clone(), "hash2"),
        ];

        let animated_options = AnimatedOptions {
            frame_pattern: r"^(?P<name>.+?)_(\d+)$".to_string(),
            min_frames: 2,
            layout: AnimationLayout::VerticalStrip,
            default_frame_duration_ms: 150,
            default_loop: false,
            ..Default::default()
        };

        let options = PackOptions {
            enabled: true,
            mode: PackMode::Animated(animated_options),
            output: OutputOptions::default(),
        };

        let packer = Packer::new(options);
        let result = packer.detect_animations(
            &assets,
            &match &packer.options.mode {
                PackMode::Animated(opts) => opts.clone(),
                _ => panic!("Expected Animated mode"),
            },
        );

        assert!(result.is_ok());
        let items = result.unwrap();
        assert_eq!(items.len(), 1);

        match &items[0] {
            PackableItem::Animated(anim) => {
                assert_eq!(anim.frame_count, 2);
                assert_eq!(anim.frame_duration_ms, 150);
                assert!(!anim.loops);
                assert_eq!(anim.strip_sprite.size.width, 2);
                assert_eq!(anim.strip_sprite.size.height, 4); // 2 frames * 2px height
            }
            _ => panic!("Expected animated item"),
        }
    }

    #[test]
    fn test_animation_detection_grid_layout() {
        let test_image_data = create_test_image_png(2, 2, [0, 0, 255, 255]);

        // 6 frames in a 3x2 grid
        let assets = (1..=6)
            .map(|i| {
                create_test_asset(
                    &format!("attack_{:02}.png", i),
                    test_image_data.clone(),
                    &format!("hash{}", i),
                )
            })
            .collect::<Vec<_>>();

        let animated_options = AnimatedOptions {
            frame_pattern: r"^(?P<name>.+?)_(\d+)$".to_string(),
            min_frames: 2,
            layout: AnimationLayout::Grid { columns: Some(3) },
            default_frame_duration_ms: 50,
            default_loop: true,
            ..Default::default()
        };

        let options = PackOptions {
            enabled: true,
            mode: PackMode::Animated(animated_options),
            output: OutputOptions::default(),
        };

        let packer = Packer::new(options);
        let result = packer.detect_animations(
            &assets,
            &match &packer.options.mode {
                PackMode::Animated(opts) => opts.clone(),
                _ => panic!("Expected Animated mode"),
            },
        );

        assert!(result.is_ok());
        let items = result.unwrap();
        assert_eq!(items.len(), 1);

        match &items[0] {
            PackableItem::Animated(anim) => {
                assert_eq!(anim.frame_count, 6);
                assert_eq!(anim.strip_sprite.size.width, 6); // 3 columns * 2px
                assert_eq!(anim.strip_sprite.size.height, 4); // 2 rows * 2px
            }
            _ => panic!("Expected animated item"),
        }
    }

    #[test]
    fn test_mixed_static_and_animated() {
        let test_image_data = create_test_image_png(2, 2, [128, 128, 128, 255]);

        let assets = vec![
            // Animation frames
            create_test_asset("walk_01.png", test_image_data.clone(), "hash1"),
            create_test_asset("walk_02.png", test_image_data.clone(), "hash2"),
            // Static sprite (doesn't match pattern)
            create_test_asset("logo.png", test_image_data.clone(), "hash3"),
        ];

        let animated_options = AnimatedOptions {
            frame_pattern: r"^(?P<name>.+?)_(\d+)$".to_string(),
            min_frames: 2,
            layout: AnimationLayout::HorizontalStrip,
            default_frame_duration_ms: 100,
            default_loop: true,
            ..Default::default()
        };

        let options = PackOptions {
            enabled: true,
            mode: PackMode::Animated(animated_options),
            output: OutputOptions::default(),
        };

        let packer = Packer::new(options);
        let result = packer.detect_animations(
            &assets,
            &match &packer.options.mode {
                PackMode::Animated(opts) => opts.clone(),
                _ => panic!("Expected Animated mode"),
            },
        );

        assert!(result.is_ok());
        let items = result.unwrap();
        assert_eq!(items.len(), 2); // 1 animation + 1 static

        let mut has_animated = false;
        let mut has_static = false;

        for item in items {
            match item {
                PackableItem::Animated(_) => {
                    has_animated = true;
                }
                PackableItem::Static(sprite) => {
                    assert_eq!(sprite.name, "logo");
                    has_static = true;
                }
            }
        }

        assert!(has_animated && has_static);
    }

    #[test]
    fn test_static_mode_ignores_animations() {
        let test_image_data = create_test_image_png(2, 2, [255, 255, 0, 255]);

        let assets = vec![
            create_test_asset("walk_01.png", test_image_data.clone(), "hash1"),
            create_test_asset("walk_02.png", test_image_data.clone(), "hash2"),
        ];

        let options = PackOptions {
            enabled: true,
            mode: PackMode::Static(StaticOptions::default()),
            output: OutputOptions::default(),
        };

        let packer = Packer::new(options);
        let result = packer.assets_to_sprites(&assets);

        assert!(result.is_ok());
        let items = result.unwrap();
        assert_eq!(items.len(), 2); // Both should be static sprites

        for item in items {
            match item {
                PackableItem::Static(_) => {}
                PackableItem::Animated(_) => panic!("Should not have animations in static mode"),
            }
        }
    }

    /// Helper function to create a test asset
    fn create_test_asset(filename: &str, data: Vec<u8>, _hash: &str) -> crate::asset::Asset {
        use std::path::PathBuf;

        // Use Asset::new() constructor which properly initializes all fields including private ones
        crate::asset::Asset::new(PathBuf::from(filename), data)
            .expect("Failed to create test asset")
    }

    /// Helper function to create a simple PNG image for testing
    fn create_test_image_png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        use image::{ImageBuffer, RgbaImage};
        use std::io::Cursor;

        let mut img: RgbaImage = ImageBuffer::new(width, height);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgba(color);
        }

        let mut buffer = Cursor::new(Vec::new());
        img.write_to(&mut buffer, image::ImageFormat::Png).unwrap();
        buffer.into_inner()
    }
}
