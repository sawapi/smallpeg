# Third-party notices

smallpeg is licensed under MIT OR Apache-2.0 and contains no third-party code.

## Photo-preset quantization table

The `QuantPreset::Photo` base table is the JPEG quantization table that
ImageMagick ships as its default (other encoders offer the same table as a
selectable preset), used here as numeric data.

## Clipped-pixel deringing (technique only)

The idea that pixels clipped at full white are free to overshoot past 255,
so that quantization ringing is clamped away by the decoder, follows
mozjpeg's "overshoot deringing". No mozjpeg code is used: smallpeg's
formulation (per-block minimum-curvature fill in 2-D under box
constraints, solved by projected SOR) and its implementation are its own,
and its parameters were chosen by its own measurements.

## Trellis quantization (technique only)

Rate-distortion optimized ("trellis") quantization of JPEG coefficients is
a published technique that mozjpeg brought into everyday JPEG encoding.
No mozjpeg code is used: smallpeg's dynamic program, its rate model and
its parameters are its own, implemented from the standard
rate-distortion formulation.

## JPEG XL recompression (from the standard)

The `jxl` feature is written from the JPEG XL standard (ISO/IEC 18181).
Its fixed tables are the standard's own values: the contexts of the
coefficient coding, the prediction of the color profile's header, and
the natural order of the coefficients. The choices an encoder is free to
make (the tree for the DC image, the block contexts, the coefficient
orders) are computed from each image by smallpeg's own methods.
