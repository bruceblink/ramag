//! 在 macOS 宿主机上为 Windows Release 预编译 GPUI DirectX 着色器。
//! 该文件与 Cargo.lock 中锁定的 gpui_windows 版本配套；依赖升级时必须同步核对。

#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]

use std::process;

fn main() {
    println!("cargo:rerun-if-env-changed=GPUI_FXC_PATH");
    println!("cargo:rerun-if-env-changed=RAMAG_FXC_EXE");
    println!("cargo:rerun-if-env-changed=RAMAG_WINE");
    println!("cargo:rerun-if-env-changed=RAMAG_WINEPATH");
    println!("cargo:rerun-if-env-changed=WINEPREFIX");

    let is_windows_target = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !is_windows_target || cfg!(debug_assertions) {
        return;
    }

    if let Err(error) = shader_compilation::compile_shaders() {
        println!("cargo:error={error}");
        process::exit(1);
    }
}

mod shader_compilation {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    const MODULES: [&str; 8] = [
        "quad",
        "shadow",
        "path_rasterization",
        "path_sprite",
        "underline",
        "monochrome_sprite",
        "subpixel_sprite",
        "polychrome_sprite",
    ];

    pub fn compile_shaders() -> Result<(), String> {
        let manifest_dir = required_path("CARGO_MANIFEST_DIR")?;
        let out_dir = required_path("OUT_DIR")?;
        let shader_path = manifest_dir.join("src/shaders.hlsl");
        let color_text_shader_path = manifest_dir.join("src/color_text_raster.hlsl");
        let fxc_path = required_path("GPUI_FXC_PATH")?;

        ensure_file(&shader_path, "GPUI shader source")?;
        ensure_file(&color_text_shader_path, "GPUI color-text shader source")?;
        ensure_file(&fxc_path, "GPUI_FXC_PATH")?;

        println!("cargo:rerun-if-changed={}", shader_path.display());
        println!(
            "cargo:rerun-if-changed={}",
            color_text_shader_path.display()
        );

        let mut bindings = String::new();
        for module in MODULES {
            compile_shader_pair(module, &shader_path, &out_dir, &fxc_path, &mut bindings)?;
        }
        compile_shader_pair(
            "emoji_rasterization",
            &color_text_shader_path,
            &out_dir,
            &fxc_path,
            &mut bindings,
        )?;

        let binding_path = out_dir.join("shaders_bytes.rs");
        fs::write(&binding_path, bindings).map_err(|error| {
            format!(
                "failed to write shader bindings {}: {error}",
                binding_path.display()
            )
        })
    }

    fn compile_shader_pair(
        module: &str,
        shader_path: &Path,
        out_dir: &Path,
        fxc_path: &Path,
        bindings: &mut String,
    ) -> Result<(), String> {
        let variants = [
            ("vertex", "vs_4_1", "VERTEX"),
            ("fragment", "ps_4_1", "FRAGMENT"),
        ];
        for (entry_suffix, target, const_suffix) in variants {
            let entry_point = format!("{module}_{entry_suffix}");
            let output_path = out_dir.join(format!("{module}_{entry_suffix}.h"));
            let const_name = format!("{}_{const_suffix}_BYTES", module.to_uppercase());
            compile_shader(
                fxc_path,
                &entry_point,
                target,
                shader_path,
                &output_path,
                &const_name,
            )?;
            append_binding(&const_name, &output_path, bindings)?;
        }
        Ok(())
    }

    fn compile_shader(
        fxc_path: &Path,
        entry_point: &str,
        target: &str,
        shader_path: &Path,
        output_path: &Path,
        const_name: &str,
    ) -> Result<(), String> {
        let output = Command::new(fxc_path)
            .arg("/T")
            .arg(target)
            .arg("/E")
            .arg(entry_point)
            .arg("/Fh")
            .arg(output_path)
            .arg("/Vn")
            .arg(const_name)
            .arg("/O3")
            .arg(shader_path)
            .output()
            .map_err(|error| format!("failed to start FXC for {entry_point}: {error}"))?;

        if output.status.success() {
            return Ok(());
        }

        Err(format!(
            "FXC failed for {entry_point} ({target}); stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }

    fn append_binding(
        const_name: &str,
        header_path: &Path,
        bindings: &mut String,
    ) -> Result<(), String> {
        let header = fs::read_to_string(header_path).map_err(|error| {
            format!(
                "failed to read FXC output {}: {error}",
                header_path.display()
            )
        })?;
        let declaration = header
            .find("const BYTE")
            .and_then(|start| header.get(start..))
            .ok_or_else(|| {
                format!(
                    "FXC output does not contain a byte array: {}",
                    header_path.display()
                )
            })?;
        let value = declaration
            .split_once('=')
            .map(|(_, value)| value.trim())
            .ok_or_else(|| format!("FXC byte array has no value: {}", header_path.display()))?;

        bindings.push_str(&format!(
            "const {const_name}: &[u8] = &{}\n",
            value.replace('{', "[").replace('}', "]")
        ));
        Ok(())
    }

    fn required_path(name: &str) -> Result<PathBuf, String> {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("required environment variable {name} is not set"))
    }

    fn ensure_file(path: &Path, label: &str) -> Result<(), String> {
        if path.is_file() {
            Ok(())
        } else {
            Err(format!("{label} is not a file: {}", path.display()))
        }
    }
}
