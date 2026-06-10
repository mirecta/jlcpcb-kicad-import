// OpenCASCADE STEP parser with per-face colors
// Exports C API for Rust FFI

#include <STEPControl_Reader.hxx>
#include <TopExp_Explorer.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Face.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <Poly_Triangulation.hxx>
#include <TopLoc_Location.hxx>
#include <BRep_Tool.hxx>
#include <Quantity_Color.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <XCAFDoc_ColorTool.hxx>
#include <TDocStd_Document.hxx>
#include <STEPCAFControl_Reader.hxx>
#include <TDF_LabelSequence.hxx>
#include <TDataStd_Name.hxx>
#include <vector>
#include <cstring>
#include <iostream>

extern "C" {

// Mesh data structure passed to Rust
struct ColoredMesh {
    float* vertices;     // interleaved: [x,y,z, nx,ny,nz, r,g,b] per vertex
    int vertex_count;
    char* error_msg;     // NULL if success
};

// Parse STEP file and extract colored triangulated mesh
ColoredMesh* step_parse_with_colors(const char* data, int data_len) {
    ColoredMesh* result = new ColoredMesh{nullptr, 0, nullptr};

    try {
        // Write data to temporary file (OCC needs file path)
        const char* temp_path = "/tmp/step_temp.stp";
        FILE* f = fopen(temp_path, "wb");
        if (!f) {
            result->error_msg = strdup("Failed to create temp file");
            return result;
        }
        fwrite(data, 1, data_len, f);
        fclose(f);

        // Use XCAF (extended CAF) reader for colors
        Handle(TDocStd_Document) doc = new TDocStd_Document("MDTV-XCAF");
        STEPCAFControl_Reader reader;

        if (reader.ReadFile(temp_path) != IFSelect_RetDone) {
            result->error_msg = strdup("Failed to read STEP file");
            return result;
        }

        if (!reader.Transfer(doc)) {
            result->error_msg = strdup("Failed to transfer STEP data");
            return result;
        }

        // Get shape and color tools
        Handle(XCAFDoc_ShapeTool) shapeTool = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
        Handle(XCAFDoc_ColorTool) colorTool = XCAFDoc_DocumentTool::ColorTool(doc->Main());

        // Get all shapes
        TDF_LabelSequence labels;
        shapeTool->GetFreeShapes(labels);

        std::vector<float> mesh_data;

        // Process each shape
        for (int i = 1; i <= labels.Length(); i++) {
            TDF_Label label = labels.Value(i);
            TopoDS_Shape shape;
            if (!shapeTool->GetShape(label, shape)) continue;

            // Triangulate the shape
            BRepMesh_IncrementalMesh mesher(shape, 0.1, Standard_False, 0.5, Standard_True);

            // Explore all faces
            for (TopExp_Explorer faceExp(shape, TopAbs_FACE); faceExp.More(); faceExp.Next()) {
                TopoDS_Face face = TopoDS::Face(faceExp.Current());
                TopLoc_Location loc;
                Handle(Poly_Triangulation) triangulation = BRep_Tool::Triangulation(face, loc);

                if (triangulation.IsNull()) continue;

                // Get face color (default gray if not found)
                Quantity_Color faceColor(0.75, 0.75, 0.75, Quantity_TOC_RGB);
                if (colorTool->GetColor(face, XCAFDoc_ColorSurf, faceColor) ||
                    colorTool->GetColor(label, XCAFDoc_ColorGen, faceColor)) {
                    // Color found!
                }

                float r = faceColor.Red();
                float g = faceColor.Green();
                float b = faceColor.Blue();

                // Get transformation
                gp_Trsf transform = loc.Transformation();

                // Extract triangles
                const TColgp_Array1OfPnt& nodes = triangulation->Nodes();
                const Poly_Array1OfTriangle& triangles = triangulation->Triangles();

                for (int t = 1; t <= triangles.Length(); t++) {
                    const Poly_Triangle& tri = triangles(t);
                    int n1, n2, n3;
                    tri.Get(n1, n2, n3);

                    gp_Pnt p1 = nodes(n1).Transformed(transform);
                    gp_Pnt p2 = nodes(n2).Transformed(transform);
                    gp_Pnt p3 = nodes(n3).Transformed(transform);

                    // Compute face normal
                    gp_Vec v1(p1, p2);
                    gp_Vec v2(p1, p3);
                    gp_Vec normal = v1.Crossed(v2);
                    if (normal.Magnitude() > 1e-9) {
                        normal.Normalize();
                    } else {
                        normal = gp_Vec(0, 0, 1);
                    }

                    // Add triangle vertices with color
                    for (const gp_Pnt& p : {p1, p2, p3}) {
                        mesh_data.push_back(p.X());
                        mesh_data.push_back(p.Y());
                        mesh_data.push_back(p.Z());
                        mesh_data.push_back(normal.X());
                        mesh_data.push_back(normal.Y());
                        mesh_data.push_back(normal.Z());
                        mesh_data.push_back(r);
                        mesh_data.push_back(g);
                        mesh_data.push_back(b);
                    }
                }
            }
        }

        // Copy to result
        result->vertex_count = mesh_data.size() / 9;
        result->vertices = new float[mesh_data.size()];
        std::memcpy(result->vertices, mesh_data.data(), mesh_data.size() * sizeof(float));

        // Cleanup temp file
        std::remove(temp_path);

    } catch (const Standard_Failure& e) {
        result->error_msg = strdup(e.GetMessageString());
    } catch (const std::exception& e) {
        result->error_msg = strdup(e.what());
    } catch (...) {
        result->error_msg = strdup("Unknown error");
    }

    return result;
}

// Free mesh memory
void step_free_mesh(ColoredMesh* mesh) {
    if (!mesh) return;
    delete[] mesh->vertices;
    free(mesh->error_msg);
    delete mesh;
}

} // extern "C"
