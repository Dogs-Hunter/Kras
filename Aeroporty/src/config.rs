use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub cache_dir: PathBuf,
    pub output_dir: PathBuf,
    pub enrichment_file: PathBuf,
    pub offline: bool,
    pub force: bool,
    pub enable_local_enrichment: bool,
    pub enable_faa: bool,
    pub faa_release: String,
    pub enable_anac: bool,
    pub anac_release: String,
    pub anac_local_dir: Option<PathBuf>,
    pub enable_phase5: bool,
}

impl Config {
    pub fn from_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            cache_dir: root.join("cache"),
            output_dir: root.join("output").join("candidate-phase5"),
            enrichment_file: root.join("data").join("enrichment.json"),
            root,
            offline: false,
            force: false,
            enable_local_enrichment: true,
            enable_faa: true,
            faa_release: "2026-10-01".into(),
            enable_anac: true,
            anac_release: "2026-10-11".into(),
            anac_local_dir: None,
            enable_phase5: true,
        }
    }

    pub fn with_output(mut self, output: Option<PathBuf>) -> Self {
        if let Some(output) = output {
            self.output_dir = if output.is_absolute() {
                output
            } else {
                self.root.join(output)
            };
        }
        self
    }
}
