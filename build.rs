// Build script to compile OpenCASCADE C++ wrapper

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Check if OCCT headers are available (Ubuntu/Debian path)
    let occt_include = PathBuf::from("/usr/include/opencascade");
    let occt_available = occt_include.exists();

    if !occt_available {
        eprintln!("\n❌ ERROR: OpenCASCADE is required but not found!\n");
        eprintln!("This application requires OpenCASCADE 7.8+ for STEP file parsing.");
        eprintln!("\nInstall it with:");
        eprintln!("  sudo apt-get install libocct-data-exchange-dev libocct-foundation-dev \\");
        eprintln!("    libocct-modeling-data-dev libocct-modeling-algorithms-dev\n");
        panic!("OpenCASCADE not found - cannot build without it");
    }

    // Ubuntu/Debian OCCT 7.8 paths (no pkg-config provided)
    let cflags_str = format!("-I{}", occt_include.display());
    let occt_libs = vec![
        // Core libraries
        "TKernel", "TKMath", "TKG2d", "TKG3d", "TKGeomBase", "TKBRep",
        // Data Exchange (STEP support)
        "TKDESTEP", "TKXSBase", "TKDE",
        // XCAF (colors and attributes)
        "TKXCAF", "TKLCAF", "TKCAF", "TKVCAF", "TKCDF",
        // Modeling and algorithms
        "TKMesh", "TKBO", "TKPrim", "TKHLR", "TKTopAlgo", "TKGeomAlgo",
        "TKShHealing", "TKOffset", "TKFillet", "TKBool",
        // Visualization
        "TKV3d", "TKService",
    ];

    // Compile C++ wrapper
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let cpp_file = "src/step_occ.cpp";
    let obj_file = out_dir.join("step_occ.o");
    let lib_file = out_dir.join("libstep_occ.a");

    println!("cargo:rerun-if-changed={}", cpp_file);

    // Compile to object file
    let compile_status = Command::new("g++")
        .args(&["-c", "-std=c++17", "-fPIC"])
        .args(cflags_str.split_whitespace())
        .arg(cpp_file)
        .arg("-o")
        .arg(&obj_file)
        .status()
        .expect("Failed to compile C++");

    if !compile_status.success() {
        panic!("C++ compilation failed");
    }

    // Create static library
    let ar_status = Command::new("ar")
        .args(&["rcs"])
        .arg(&lib_file)
        .arg(&obj_file)
        .status()
        .expect("Failed to create static library");

    if !ar_status.success() {
        panic!("Failed to create static library");
    }

    // Tell cargo where to find the library
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=step_occ");

    // Link OCCT libraries
    for lib in &occt_libs {
        println!("cargo:rustc-link-lib=dylib={}", lib);
    }

    // Link standard C++ library
    println!("cargo:rustc-link-lib=dylib=stdc++");

    println!("cargo:warning=✓ OpenCASCADE wrapper compiled successfully!");
}
