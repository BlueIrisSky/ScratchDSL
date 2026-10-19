use crate::{
    Error, Result,
    config::{self, Asset, BuildConfig},
    model::GeneratedProject,
    registry::Registry,
};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Cursor, Write},
    path::Path,
};

pub fn compile(config: &BuildConfig) -> Result<GeneratedProject> {
    let mut registry = Registry::new();
    crate::extensions::load_extensions(&config.extensions, &mut registry)?;
    let project = crate::project::load(&config.entry, &config.src_dir)?;
    let analysis = crate::analysis::bind::bind_project(&project, &registry)?;
    let mut generated = crate::codegen::generate_bound(&analysis)?;
    let mut targets = generated.json["targets"]
        .as_array_mut()
        .ok_or_else(|| Error::new("内部错误：缺少 targets"))?
        .clone();
    for (index, target) in targets.iter_mut().enumerate() {
        let is_stage = target["isStage"] == true;
        let source = if is_stage {
            &project.stage
        } else {
            &project.sprites[index - 1]
        };
        let properties = source.init.as_ref();
        let at_source = |error: Error| match &source.declaration_span {
            Some(span) => error.at(span),
            None => error,
        };
        let costumes = if is_stage {
            &config.config.stage.backdrops[..]
        } else {
            properties.map(|p| &p.costumes[..]).unwrap_or(&[])
        };
        let sounds = if is_stage {
            &config.config.stage.sounds[..]
        } else {
            properties.map(|p| &p.sounds[..]).unwrap_or(&[])
        };
        target["costumes"] = Value::Array(
            costume_assets(costumes, &config.base_dir, is_stage, &mut generated)
                .map_err(at_source)?,
        );
        target["sounds"] = Value::Array(
            sound_assets(sounds, &config.base_dir, &mut generated).map_err(at_source)?,
        );
        target["currentCostume"] = json!(0);
        if !is_stage {
            if let Some(p) = properties {
                for (key, value) in [
                    ("x", p.x),
                    ("y", p.y),
                    ("size", p.size),
                    ("direction", p.direction),
                ] {
                    target[key] = json!(value);
                }
                target["visible"] = json!(p.visible);
                target["rotationStyle"] = json!(p.rotation_style);
            }
        } else if let Some(runtime) = &config.config.runtime {
            let mut settings = json!({});
            if let Some(v) = runtime.fps {
                settings["framerate"] = json!(v);
            }
            if let Some(v) = runtime.stage_width {
                settings["width"] = json!(v);
            }
            if let Some(v) = runtime.stage_height {
                settings["height"] = json!(v);
            }
            if let Some(v) = runtime.hq_pen {
                settings["hq"] = json!(v);
            }
            let mut options = json!({});
            if let Some(v) = runtime.max_clones {
                options["maxClones"] = json!(v);
            }
            if let Some(v) = runtime.fencing {
                options["fencing"] = json!(v);
            }
            if let Some(v) = runtime.misc_limits {
                options["miscLimits"] = json!(v);
            }
            if !options.as_object().unwrap().is_empty() {
                settings["runtimeOptions"] = options;
            }
            if target.get("comments").and_then(Value::as_object).is_none() {
                target["comments"] = json!({});
            }
            target["comments"]["sbdsl_runtime"] = json!({"blockId":null,"x":0,"y":0,"width":350,"height":200,"minimized":true,"text":format!("ScratchDSL TurboWarp settings\n{} // _twconfig_",settings)});
        }
    }
    generated.json["targets"] = Value::Array(targets);
    generated.json["extensions"] = json!(registry.extension_sources.keys().collect::<Vec<_>>());
    let urls = registry
        .extension_sources
        .iter()
        .map(|(id, source)| {
            (
                id.clone(),
                json!(format!(
                    "data:application/javascript;charset=utf-8;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(source.as_bytes())
                )),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    generated.json["extensionURLs"] = Value::Object(urls);
    if let Some(name) = &config.config.name {
        generated.json["meta"]["scratchDSL"]["name"] = json!(name);
    }
    Ok(generated)
}

pub fn write_sb3(project: &GeneratedProject, output: &Path) -> Result<()> {
    let bytes = sb3_bytes(project)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    // Finish and validate the complete ZIP before touching the requested output.
    fs::write(output, bytes).map_err(|e| Error::new(format!("写入 {}: {e}", output.display())))
}

pub fn sb3_bytes(project: &GeneratedProject) -> Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("project.json", options)?;
    zip.write_all(&serde_json::to_vec(&project.json)?)?;
    for (name, data) in &project.assets {
        zip.start_file(name, options)?;
        zip.write_all(data)?;
    }
    Ok(zip.finish()?.into_inner())
}

fn asset_data(asset: &Asset, base: &Path) -> Result<(Vec<u8>, String)> {
    let path = config::resolve(base, &asset.path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = fs::read(&path).map_err(|e| Error::new(format!("资源 {}: {e}", path.display())))?;
    Ok((bytes, ext))
}

fn pack_asset(bytes: Vec<u8>, ext: &str, generated: &mut GeneratedProject) -> (String, String) {
    let hash = format!("{:x}", md5::compute(&bytes));
    let name = format!("{hash}.{ext}");
    generated.assets.insert(name.clone(), bytes);
    (hash, name)
}

fn costume_assets(
    assets: &[Asset],
    base: &Path,
    stage: bool,
    generated: &mut GeneratedProject,
) -> Result<Vec<Value>> {
    if assets.is_empty() {
        let (w, h) = if stage { (480, 360) } else { (1, 1) };
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\"></svg>"
        );
        let (hash, md5ext) = pack_asset(svg.into_bytes(), "svg", generated);
        return Ok(vec![
            json!({"name":"空白","assetId":hash,"md5ext":md5ext,"dataFormat":"svg","bitmapResolution":1,"rotationCenterX":w as f64/2.,"rotationCenterY":h as f64/2.}),
        ]);
    }
    assets.iter().map(|asset| {
        let (bytes,ext) = asset_data(asset,base)?;
        let (w,h) = match ext.as_str() {
            "svg" => svg_dimensions(&bytes)?,
            "png" => {
                let decoder = png::Decoder::new(Cursor::new(&bytes));
                let mut reader = decoder.read_info().map_err(|e| Error::new(format!("无效 PNG: {e}")))?;
                let mut decoded = vec![0; reader.output_buffer_size()];
                let info = reader.next_frame(&mut decoded).map_err(|e| Error::new(format!("无效 PNG: {e}")))?;
                (info.width as f64, info.height as f64)
            },
            _ => return Err(Error::new(format!("造型 {} 必须是有效的 SVG 或 PNG",asset.path.display()))),
        };
        let resolution = asset.bitmap_resolution.unwrap_or(1);
        if resolution == 0 {return Err(Error::new("bitmapResolution 不能为 0"));}
        let (hash,md5ext) = pack_asset(bytes,&ext,generated);
        Ok(json!({"name":asset.name,"assetId":hash,"md5ext":md5ext,"dataFormat":ext,"bitmapResolution":resolution,
            "rotationCenterX":asset.rotation_center_x.unwrap_or(w/2.),"rotationCenterY":asset.rotation_center_y.unwrap_or(h/2.)}))
    }).collect()
}

fn svg_dimensions(bytes: &[u8]) -> Result<(f64, f64)> {
    let source = std::str::from_utf8(bytes).map_err(|_| Error::new("SVG 必须使用 UTF-8"))?;
    let doc =
        roxmltree::Document::parse(source).map_err(|e| Error::new(format!("无效 SVG: {e}")))?;
    let svg = doc.root_element();
    if svg.tag_name().name() != "svg" {
        return Err(Error::new("SVG 缺少 svg 根元素"));
    }
    let dimension = |s: &str| {
        s.trim()
            .trim_end_matches("px")
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.)
    };
    if let Some(size) = svg
        .attribute("width")
        .and_then(dimension)
        .zip(svg.attribute("height").and_then(dimension))
    {
        return Ok(size);
    }
    if let Some(view) = svg.attribute("viewBox") {
        let nums = view
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::new("SVG viewBox 无效"))?;
        if nums.len() == 4 && nums.iter().all(|n| n.is_finite()) && nums[2] > 0. && nums[3] > 0. {
            return Ok((nums[2], nums[3]));
        }
    }
    Err(Error::new("SVG 需要正数 width/height 或有效 viewBox"))
}

fn sound_assets(
    assets: &[Asset],
    base: &Path,
    generated: &mut GeneratedProject,
) -> Result<Vec<Value>> {
    use symphonia::core::{
        codecs::DecoderOptions,
        formats::FormatOptions,
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::MetadataOptions,
        probe::Hint,
    };
    assets.iter().map(|asset| {
        let (bytes,ext) = asset_data(asset,base)?;
        if !["wav","mp3"].contains(&ext.as_str()) {return Err(Error::new("音频仅支持 WAV / MP3"));}
        let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes.clone())),MediaSourceStreamOptions::default());
        let mut hint = Hint::new(); hint.with_extension(&ext);
        let mut format = symphonia::default::get_probe().format(&hint,stream,&FormatOptions::default(),&MetadataOptions::default()).map_err(|e|Error::new(format!("音频 {}: {e}",asset.path.display())))?.format;
        let track = format.default_track().ok_or_else(||Error::new("音频缺少音轨"))?;
        let id = track.id;
        let mut decoder = symphonia::default::get_codecs().make(&track.codec_params,&DecoderOptions::default()).map_err(|e|Error::new(format!("音频解码器: {e}")))?;
        let mut rate = track.codec_params.sample_rate.unwrap_or(0);
        let mut frames = 0u64;
        loop {
            match format.next_packet() {
                Ok(packet) if packet.track_id() == id => {
                    let audio = decoder.decode(&packet).map_err(|e|Error::new(format!("音频解码 {}: {e}",asset.path.display())))?;
                    rate = audio.spec().rate; frames += audio.frames() as u64;
                }
                Ok(_) => {},
                Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(Error::new(format!("读取音频: {e}"))),
            }
        }
        if rate == 0 || frames == 0 {return Err(Error::new("音频为空或无法确定采样率"));}
        let (hash,md5ext) = pack_asset(bytes,&ext,generated);
        Ok(json!({"name":asset.name,"assetId":hash,"md5ext":md5ext,"dataFormat":ext,"format":"","rate":rate,"sampleCount":frames}))
    }).collect()
}
