use anyhow::Result;
use oxipng::Options;
use std::path::Path;

pub fn optimize_png(data: &[u8]) -> Result<Vec<u8>> {
    let options = Options::default();

    match oxipng::optimize_from_memory(data, &options) {
        Ok(optimized) => Ok(optimized),
        Err(_) => Ok(data.to_vec()),
    }
}

pub fn should_optimize(path: &Path, optimize_flag: bool) -> bool {
    if !optimize_flag {
        return false;
    }

    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
}
