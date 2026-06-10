// Rust FFI bindings for OpenCASCADE STEP parser

use std::ffi::CStr;
use std::ptr;

#[repr(C)]
struct ColoredMesh {
    vertices: *mut f32,
    vertex_count: i32,
    error_msg: *mut libc::c_char,
}

extern "C" {
    fn step_parse_with_colors(data: *const u8, data_len: i32) -> *mut ColoredMesh;
    fn step_free_mesh(mesh: *mut ColoredMesh);
}

/// Parse STEP file with OpenCASCADE and extract per-face colors
/// Returns vertex data as: [x,y,z, nx,ny,nz, r,g,b] per vertex
pub fn parse_step_occ(data: &[u8]) -> Result<Vec<f32>, String> {
    unsafe {
        let mesh_ptr = step_parse_with_colors(data.as_ptr(), data.len() as i32);
        if mesh_ptr.is_null() {
            return Err("Failed to allocate mesh".to_string());
        }

        let mesh = &*mesh_ptr;

        // Check for error
        if !mesh.error_msg.is_null() {
            let error = CStr::from_ptr(mesh.error_msg)
                .to_string_lossy()
                .into_owned();
            step_free_mesh(mesh_ptr);
            return Err(error);
        }

        // Copy vertex data
        let vertex_data_len = (mesh.vertex_count as usize) * 9;
        let mut vertices = Vec::with_capacity(vertex_data_len);
        if !mesh.vertices.is_null() && mesh.vertex_count > 0 {
            vertices.extend_from_slice(std::slice::from_raw_parts(
                mesh.vertices,
                vertex_data_len,
            ));
        }

        step_free_mesh(mesh_ptr);

        if vertices.is_empty() {
            Err("No geometry found in STEP file".to_string())
        } else {
            Ok(vertices)
        }
    }
}

/// Check if OpenCASCADE support is available
pub fn occ_available() -> bool {
    // This is set by build.rs - if the library was built, OCC is available
    cfg!(feature = "opencascade")
}
