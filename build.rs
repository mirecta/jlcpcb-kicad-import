// Build script to compile OpenCASCADE C++ wrapper

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Check if OCCT is available
    let occt_available = Command::new("pkg-config")
        .args(&["--exists", "occt"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !occt_available {
        println!("cargo:warning=OpenCASCADE not found - STEP colors will use fallback (dominant per shell)");
        println!("cargo:warning=To enable full STEP color support, install:");
        println!("cargo:warning=  sudo apt-get install libocct-data-exchange-dev libocct-foundation-dev \\");
        println!("cargo:warning=    libocct-modeling-data-dev libocct-modeling-algorithms-dev");
        return;
    }

    // Get OCCT compile flags
    let cflags = Command::new("pkg-config")
        .args(&["--cflags", "occt"])
        .output()
        .expect("Failed to get OCCT cflags");
    let cflags_str = String::from_utf8(cflags.stdout).unwrap();

    // Get OCCT link flags
    let libs = Command::new("pkg-config")
        .args(&["--libs", "occt"])
        .output()
        .expect("Failed to get OCCT libs");
    let libs_str = String::from_utf8(libs.stdout).unwrap();

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
    for lib in libs_str.split_whitespace() {
        if lib.starts_with("-l") {
            println!("cargo:rustc-link-lib={}", &lib[2..]);
        } else if lib.starts_with("-L") {
            println!("cargo:rustc-link-search=native={}", &lib[2..]);
        }
    }

    // Link standard C++ library
    println!("cargo:rustc-link-lib=stdc++");

    println!("cargo:warning=OpenCASCADE wrapper compiled successfully!");
}
