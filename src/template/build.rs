use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("data.rs");
    
    // Check if we have embedded data files
    let zip_data_path = Path::new("embedded_data.zip");
    let build_id_path = Path::new("build_id.txt");
    
    if zip_data_path.exists() && build_id_path.exists() {
        // Read the build ID
        let build_id = fs::read_to_string(build_id_path)
            .expect("Failed to read build ID");
        
        // Copy the zip file to the OUT_DIR so include_bytes! can find it
        let out_zip_path = Path::new(&out_dir).join("embedded_data.zip");
        fs::copy(zip_data_path, &out_zip_path)
            .expect("Failed to copy embedded data to OUT_DIR");
        
        // Generate the data.rs file with embedded data
        let data_rs_content = format!(
            r#"
// Generated at build time - contains embedded application data
const ZIP_DATA: &[u8] = include_bytes!("embedded_data.zip");
const BUILD_ID: &str = "{}";
"#,
            build_id.trim()
        );
        
        fs::write(&dest_path, data_rs_content)
            .expect("Failed to write data.rs");
    } else {
        // Generate placeholder data for template compilation
        let data_rs_content = r#"
// Placeholder data for template compilation
const ZIP_DATA: &[u8] = &[];
const BUILD_ID: &str = "template";
"#;
        
        fs::write(&dest_path, data_rs_content)
            .expect("Failed to write placeholder data.rs");
    }
    
    // Tell Cargo to rerun this script if the embedded data changes
    println!("cargo:rerun-if-changed=embedded_data.zip");
    println!("cargo:rerun-if-changed=build_id.txt");
}
