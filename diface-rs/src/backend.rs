//! Pluggable defacing backends.
//!
//! A backend receives a fully reconstructed [`Volume`] and returns a [`Mask`]
//! marking the voxels that should be blanked. Backends are deliberately
//! decoupled from DICOM I/O so that richer strategies (atlas registration,
//! ONNX segmentation models, or external tools) can be added without touching
//! the series loader or writer.

use crate::volume::{Mask, Volume};

/// A strategy for deciding which voxels contain identifiable facial anatomy.
pub trait DefacingBackend: Send + Sync {
    /// Short human readable identifier (used in reports and the CLI).
    fn name(&self) -> &str;

    /// Decide which voxels to remove for the given volume.
    fn compute_mask(&self, volume: &Volume) -> Result<Mask, String>;

    /// Optional one-line description for `--help` / reports.
    fn description(&self) -> String {
        String::new()
    }
}

/// Selects a backend and its parameters.
#[derive(Clone, Debug)]
pub enum BackendKind {
    /// Fast, dependency-free geometric masking. See
    /// [`crate::geometric::GeometricParams`].
    Geometric(crate::geometric::GeometricParams),
    /// Template/atlas-based masking. See [`crate::atlas::AtlasParams`].
    Atlas(crate::atlas::AtlasSpec),
    /// In-house head/brain segmentation. See
    /// [`crate::segmentation::SegBackendParams`].
    Segmentation(crate::segmentation::SegBackendParams),
}

impl Default for BackendKind {
    fn default() -> Self {
        BackendKind::Geometric(crate::geometric::GeometricParams::default())
    }
}

impl BackendKind {
    /// Build the concrete backend.
    pub fn build(&self) -> Box<dyn DefacingBackend> {
        match self {
            BackendKind::Geometric(params) => {
                Box::new(crate::geometric::GeometricBackend::new(params.clone()))
            }
            BackendKind::Atlas(spec) => Box::new(spec.build()),
            BackendKind::Segmentation(params) => Box::new(
                crate::segmentation::SegmentationBackend::new(params.clone()),
            ),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            BackendKind::Geometric(_) => "geometric",
            BackendKind::Atlas(_) => "atlas",
            BackendKind::Segmentation(_) => "segmentation",
        }
    }
}
