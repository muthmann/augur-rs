# Focus Metrics Plugin

## Summary

The `Focus Metrics` plugin estimates focus quality from either localization results or the preview frame itself. It keeps a rolling metric history, draws a lightweight trend plot in the analysis panel, and reports a coarse focus-quality indicator.

## Methods

### Mean PSF Sigma

Primary localization-driven metric.

- consumes `LocalizationResults`
- averages fitted `sigma_x` and `sigma_y`
- lower mean sigma indicates sharper focus

### FFT High Frequency

Standalone sharpness metric.

- runs on the preview frame without localization dependency
- downsamples the frame for responsiveness
- computes a 2D FFT and integrates power in a high-frequency ring
- higher integrated power indicates sharper focus

### Astigmatic Ratio

Directional metric for future 3D SMLM work.

- consumes `LocalizationResults`
- tracks `sigma_x / sigma_y`
- values near `1.0` indicate symmetric focus

## Settings

The plugin exposes:

- active method selection
- history depth
- scale in `nm/px`
- sigma filter range in nanometers

## Dependency Behavior

- `Mean PSF sigma` depends on `Molecule Localization`
- `Astigmatic ratio` depends on `Molecule Localization`
- `FFT high frequency` runs standalone

When a localization-dependent method is selected without the localization plugin, the GUI surfaces that dependency in both the analysis panel and the main warning area.
