# Trident's image-webp patch

This is the published `image-webp` 0.2.4 crate (SHA-256
`525e9ff3e1a4be2fbea1fdf0e98686a6d98b4d8f937e1bf7402245af1909e8c3`).
The only source change is in `src/alpha_blending.rs`: an opaque pixel in a
blended animation frame replaces its destination exactly. Passing it through
the fixed-point blend would darken each nonzero channel by one.

See [upstream issue #196](https://github.com/image-rs/image-webp/issues/196).
Drop the workspace override when a released decoder passes
`cargo test -p term-app show_uploads_lossless_blended_webp_pixels_exactly`.
