# TIFF Stack Export

## Summary

`augur-gui` can now batch-export replay data into a multi-page TIFF stack from the `File` menu.
Each page stores one accumulation window as a 16-bit grayscale image of total ON+OFF event counts,
and the export dialog supports time-range selection, replay-time acquisition overrides, and
optional cropping to the current hardware ROI.

## Workflow

During replay, open `File -> Export TIFF Stack…` to configure the batch export.

The dialog exposes:

- start / end time sliders in seconds over the current replay duration
- acquisition time in milliseconds, pre-filled from the current global setting
- a `Crop to current ROI` toggle using `CameraConfig::roi`
- an output path field with a `Browse…` save dialog
- an estimated frame-count label

When you click `Export`, Augur runs the stack export on a background thread and writes a multi-page
TIFF where each page corresponds to one accumulation window in the selected time range.

## Data Model

- exported pages are 16-bit grayscale
- each pixel stores total ON+OFF event counts for that accumulation window
- display-only viewer modes such as red/blue polarity and time surface do not change the exported
  pixel values
- if ROI crop is enabled, the TIFF page dimensions match the current ROI dimensions

## Replay Sources

The export path supports both replay backends already used by the GUI:

- decoded replay files (`.csv`, `.bin`, `.npy`, optional `.h5` / `.hdf5`) export directly from the
  cached `Arc<Vec<CdEvent>>`
- raw `.raw` EVT3 replays reopen the file at `ReplayFileInfo::data_offset`, decode with
  `Evt3CorePreviewDecoder`, and feed the same accumulation logic used by decoded replays

## Files

| File | Role |
|---|---|
| `augur-gui/src/export.rs` | TIFF-stack accumulation and multi-page encoder path for decoded and raw replay sources |
| `augur-gui/src/export_dialog.rs` | modal export-dialog state, validation, and UI |
| `augur-gui/src/app.rs` | File-menu wiring, background task lifecycle, replay acq-time fix |
| `docs/gui.md` | user-facing replay export workflow |

## Verification

- `cargo build -p augur-gui`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- manual GUI check recommended for:
  - exporting a decoded replay to verify the multi-page TIFF opens correctly in ImageJ/Fiji
  - exporting a raw EVT3 replay to confirm the decode-backed batch path matches replay timing
  - confirming replay-time acquisition edits change the next frame without disabling the control
