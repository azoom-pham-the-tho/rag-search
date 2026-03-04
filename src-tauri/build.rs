fn main() {
    // Tự động tìm Tesseract trên macOS (Homebrew) và Windows (vcpkg/chocolatey)
    #[cfg(target_os = "macos")]
    {
        // Homebrew paths (Intel và Apple Silicon)
        let homebrew_paths = [
            "/opt/homebrew/lib", // Apple Silicon
            "/usr/local/lib",    // Intel Mac
            "/opt/homebrew/opt/tesseract/lib",
            "/usr/local/opt/tesseract/lib",
        ];

        for path in &homebrew_paths {
            if std::path::Path::new(path).exists() {
                println!("cargo:rustc-link-search=native={}", path);
            }
        }

        // Leptonica (Tesseract dependency)
        let lept_paths = [
            "/opt/homebrew/lib",
            "/usr/local/lib",
            "/opt/homebrew/opt/leptonica/lib",
        ];
        for path in &lept_paths {
            if std::path::Path::new(path).exists() {
                println!("cargo:rustc-link-search=native={}", path);
            }
        }

        // Cảnh báo nếu tesseract chưa được cài
        if !homebrew_paths.iter().any(|p| {
            std::path::Path::new(p).join("libtesseract.dylib").exists()
                || std::path::Path::new(p)
                    .join("libtesseract.5.dylib")
                    .exists()
        }) {
            println!("cargo:warning=Tesseract không tìm thấy! Chạy: brew install tesseract tesseract-lang");
        }
    }

    #[cfg(target_os = "windows")]
    {
        // vcpkg default path
        if let Ok(vcpkg_root) = std::env::var("VCPKG_ROOT") {
            println!(
                "cargo:rustc-link-search=native={}/installed/x64-windows/lib",
                vcpkg_root
            );
        }
    }

    tauri_build::build()
}
