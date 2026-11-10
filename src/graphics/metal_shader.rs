//! Translate the same GLSL 450 shaders used by Vulkan into native MSL.
use super::*;
use spirv_cross2::{
    compile::{
        msl::{BindTarget, MslVersion, ResourceBinding},
        CompilableTarget,
    },
    spirv::ExecutionModel,
    targets::Msl,
    Compiler, Module,
};

pub fn translate(source: &str, stage: shaderc::ShaderKind) -> Result<String, String> {
    let compiler = shaderc::Compiler::new().map_err(|e| e.to_string())?;
    let mut options = shaderc::CompileOptions::new().map_err(|e| e.to_string())?;
    options.set_target_env(
        shaderc::TargetEnv::Vulkan,
        shaderc::EnvVersion::Vulkan1_1 as u32,
    );
    options.set_auto_bind_uniforms(true);
    options.set_auto_map_locations(true);
    let spirv = compiler
        .compile_into_spirv(source, stage, "metal.glsl", "main", Some(&options))
        .map_err(|e| e.to_string())?;
    let mut compiler =
        Compiler::<Msl>::new(Module::from_words(spirv.as_binary())).map_err(|e| e.to_string())?;
    let mut options = Msl::options();
    options.version = MslVersion::from((2, 0));
    // Preserve OpenGL's bottom-first render-target rows and depth convention.
    // Texture uploads and post passes then use the same UVs on every backend.
    options.common.flip_vertex_y = true;
    options.common.fixup_clipspace = true;
    let stage = if stage == shaderc::ShaderKind::Vertex {
        ExecutionModel::Vertex
    } else {
        ExecutionModel::Fragment
    };
    for binding in 0..=MAX_SHADERSTAGE_IMAGES as u32 {
        let index = binding.saturating_sub(1);
        compiler
            .add_resource_binding(
                stage,
                ResourceBinding::from_qualified(0, binding),
                &BindTarget {
                    buffer: 0,
                    texture: index,
                    sampler: index,
                    count: None,
                },
            )
            .map_err(|e| e.to_string())?;
    }
    compiler
        .compile(&options)
        .map(|artifact| artifact.to_string())
        .map_err(|e| e.to_string())
}

/// Repack macroquad's tightly packed uniforms to std140. Arrays have at least
/// 16-byte alignment and stride, including scalar arrays.
pub fn pack(layout: &UniformBlockLayout, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut source = 0;
    for uniform in &layout.uniforms {
        let size = uniform.uniform_type.size() as usize;
        let alignment = match uniform.uniform_type {
            UniformType::Float1 | UniformType::Int1 => 4,
            UniformType::Float2 | UniformType::Int2 => 8,
            _ => 16,
        };
        let alignment = if uniform.array_count > 1 {
            16
        } else {
            alignment
        };
        let offset = (output.len() + alignment - 1) / alignment * alignment;
        output.resize(offset, 0);
        let stride = if uniform.array_count > 1 {
            (size + 15) / 16 * 16
        } else {
            size
        };
        for _ in 0..uniform.array_count {
            assert!(
                source + size <= input.len(),
                "uniform data is shorter than its layout"
            );
            output.extend_from_slice(&input[source..source + size]);
            output.resize(output.len() + stride - size, 0);
            source += size;
        }
    }
    output.resize((output.len() + 15) / 16 * 16, 0);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scalar_arrays_keep_std140_stride() {
        let layout = UniformBlockLayout {
            uniforms: vec![
                UniformDesc::new("values", UniformType::Float1).array(2),
                UniformDesc::new("tail", UniformType::Float1),
            ],
        };
        let packed = pack(
            &layout,
            &[1u32, 2, 3]
                .iter()
                .flat_map(|n| n.to_ne_bytes())
                .collect::<Vec<_>>(),
        );
        assert_eq!(packed.len(), 48);
        assert_eq!(&packed[0..4], &1u32.to_ne_bytes());
        assert_eq!(&packed[16..20], &2u32.to_ne_bytes());
        assert_eq!(&packed[32..36], &3u32.to_ne_bytes());
    }
    #[test]
    fn translates_uniforms_and_samplers() {
        let msl = translate(
            r#"#version 450 core
layout(location=0) in vec2 uv;
layout(location=0) out vec4 color;
layout(set=0,binding=0) uniform U { vec4 tint; };
layout(set=0,binding=1) uniform sampler2D tex;
void main() { color = texture(tex, uv) * tint; }"#,
            shaderc::ShaderKind::Fragment,
        )
        .unwrap();
        assert!(msl.contains("[[buffer(0)]]"));
        assert!(msl.contains("[[texture(0)]]"));
        assert!(msl.contains("[[sampler(0)]]"));
    }
    #[test]
    fn sampler_gaps_keep_binding_slots() {
        let msl = translate(
            r#"#version 450 core
layout(location=0) in vec2 uv;
layout(location=0) out vec4 color;
layout(set=0,binding=3) uniform sampler2D tex;
void main() { color = texture(tex, uv); }"#,
            shaderc::ShaderKind::Fragment,
        )
        .unwrap();
        assert!(msl.contains("[[texture(2)]]"));
        assert!(msl.contains("[[sampler(2)]]"));
    }
    #[test]
    fn vertex_attributes_remain_unnormalized() {
        let msl = translate(
            r#"#version 450 core
layout(location=0) in vec3 position;
layout(location=1) in uvec4 color0;
layout(location=0) out vec4 color;
layout(set=0,binding=0) uniform U { mat4 Projection; mat4 Model; };
void main() { gl_Position = Projection * Model * vec4(position,1); color = vec4(color0) / 255.; }"#,
            shaderc::ShaderKind::Vertex,
        )
        .unwrap();
        assert!(msl.contains("[[attribute(0)]]"));
        assert!(msl.contains("[[attribute(1)]]"));
        assert!(msl.contains("[[buffer(0)]]"));
        assert!(msl.contains("Invert Y-axis for Metal"));
        assert!(msl.contains("Adjust clip-space for Metal"));
    }
}
