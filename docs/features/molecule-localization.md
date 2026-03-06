# Molecule Localization Plugin

## Summary

The `Molecule Localization` plugin detects candidate emitters in the preview stream, fits sub-pixel Gaussian spots, renders crosshair markers, and publishes `LocalizationResults` into the plugin context for downstream consumers.

The implementation stays entirely in the GUI plugin layer.

## Pipeline

For each preview frame, the plugin performs:

1. reconstruct an analysis image from raw events when available, otherwise fall back to preview counts
2. apply a trous wavelet smoothing with the thesis kernels `g1` and `g2`
3. compute `F1 = V0 - V1` and `F2 = V1 - V2`
4. threshold `F2` at `n * sigma(F1)`
5. keep 8-connected local maxima as spot candidates
6. compute a center-of-mass seed per candidate
7. extract a `9x9` fit ROI by default
8. run a damped least-squares elliptical Gaussian fit
9. reject fits outside the configured sigma and uncertainty filters

## Settings

The plugin exposes:

- wavelet threshold factor `n`
- fitting radius in pixels
- initial sigma in pixels
- scale in `nm/px` for filter display
- sigma min/max filter in nanometers
- max xy uncertainty in nanometers
- overlay toggle

## Output

Per frame the plugin:

- publishes `LocalizationResults`
- renders crosshair overlays at accepted fit positions
- shows accepted-vs-candidate counts in the analysis panel

## Notes

- The current implementation reconstructs a signed time-weighted image from raw events when raw-event transport is enabled.
- Timestamp assignment uses the fitted neighborhood in the current frame window.
- All localization types and fitting code remain outside `augur-core`.
