//! Statically links libvpx, libaom, Opus and libyuv.
//!
//! The sources are the tarballs under `libs/third_party/tarballs`. `build.rs`
//! compiles them with the C toolchain cargo already selected, including a
//! Yocto/Buildroot cross compiler. Nothing is downloaded and no prebuilt
//! shared object is used.
