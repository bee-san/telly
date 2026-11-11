// Picture filters for the player. Each is a chain of WebGL passes over the
// video's frame, drawn to a canvas that covers the video. The subtitles are
// HTML above both, so no filter touches them.
// The shaders, and where they come from, are in ./shaders.

const sources = import.meta.glob("./shaders/*.glsl", { query: "?raw", import: "default", eager: true }) as Record<string, string>;

/** A shader's source. michadame's are desktop OpenGL: only the first line differs from WebGL 2's. */
const glsl = (name: string) =>
  sources[`./shaders/${name}.glsl`].replace("#version 330 core", "#version 300 es\nprecision highp float;\nprecision highp sampler2D;");

export const filters = [
  { id: "none", name: "None", note: "" },
  { id: "deband", name: "Deband", note: "Smooths colour bands" },
  { id: "sharpen", name: "Sharpen", note: "Crisper lines" },
  { id: "anime4k-s", name: "Anime4K S", note: "Fast" },
  { id: "anime4k-m", name: "Anime4K M", note: "Balanced" },
  { id: "anime4k-l", name: "Anime4K L", note: "Best, slowest" },
  { id: "crt-trinitron", name: "Trinitron", note: "Sony · 90s" },
  { id: "crt-pvm", name: "PVM-20M2", note: "Sony · broadcast" },
  { id: "crt-dorm", name: "Dorm TV", note: "13″ · early 90s" },
];

type Size = [number, number];
/** The video's size, the picture's as it stands in the chain, the canvas's, and the size the televisions work at. */
interface Sizes {
  source: Size;
  main: Size;
  out: Size;
  work: Size;
}
interface Pass {
  shader: string;
  /** Each sampler in the shader, and the texture it reads. MAIN is the picture so far. */
  binds: Record<string, string>;
  /** The texture it draws, or null for the canvas. */
  save: string | null;
  size: (s: Sizes) => Size;
  uniforms?: (s: Sizes) => Record<string, number | number[]>;
}

/** One shader over the picture, at its own size. */
const simple = (name: string): Pass[] => [
  { shader: glsl(name), binds: { input_tex: "MAIN" }, save: "MAIN", size: (s) => s.main, uniforms: () => ({ seed: Math.random() }) },
];

/** Anime4K's 2× upscaler: its passes say what they read and draw in mpv's //! lines. */
function anime4k(model: string, lastConv: number): Pass[] {
  const names = ["in", ...Array.from({ length: lastConv - 1 }, (_, i) => `conv${i + 2}`), "out"];
  return names.map((name) => {
    const shader = glsl(`fs_anime4k_${model}_${name}`);
    const binds: Record<string, string> = {};
    for (const [, tex] of shader.matchAll(/^\/\/!BIND (\w+)/gm)) binds[tex === "MAIN" ? "input_tex" : tex] = tex;
    const save = /^\/\/!SAVE (\w+)/m.exec(shader)![1];
    return { shader, binds, save, size: (s) => (name === "out" ? [s.main[0] * 2, s.main[1] * 2] : s.main) };
  });
}

/** michadame's Halo television, with one of its presets. Its last three passes are at the canvas's size. */
function halo(preset: Record<string, number>): Pass[] {
  const view = (s: Sizes) => ({
    outputResolution: s.out,
    SourceSize: s.work,
    horizontal_stretch: 1,
    halo_zoom: 100,
    border_crop: [0, 0, 0, 0],
    background_color: [0, 0, 0],
    ...preset,
  });
  const blur = (name: string, from: string, to: string): Pass => ({
    shader: glsl(`fs_halo_${name}`),
    binds: { video_texture: from },
    save: to,
    size: (s) => s.work,
    uniforms: (s) => ({ SourceSize: s.work }),
  });
  return [
    // The television's passes work in linear light, and the last of them puts the gamma back.
    {
      shader: glsl("fs_linearize"),
      binds: { srgb_tex: "MAIN" },
      save: "Linear",
      size: (s) => s.source,
      uniforms: () => ({ brightness: 1, gamma: 1, overscan_offset: [0, 0], underscan_stretch: [1, 1] }),
    },
    blur("linearize", "Linear", "Linearize"),
    blur("glow_h", "Linearize", "GlowH"),
    blur("glow_v", "GlowH", "Glow"),
    blur("bloom_h", "Linearize", "BloomH"),
    blur("bloom_v", "BloomH", "Bloom"),
    { shader: glsl("fs_halo_scanlines"), binds: { LinearizePass: "Linearize" }, save: "Scanlines", size: (s) => s.out, uniforms: view },
    {
      shader: glsl("fs_halo_composite"),
      binds: { ScanlinesPass: "Scanlines", LinearizePass: "Linearize", GlowPass: "Glow", BloomPass: "Bloom" },
      save: "Composite",
      size: (s) => s.out,
      uniforms: view,
    },
    { shader: glsl("fs_halo_final"), binds: { DeconvPass: "Composite" }, save: null, size: (s) => s.out, uniforms: view },
  ];
}

const chains: Record<string, () => Pass[]> = {
  "deband": () => simple("deband"),
  "sharpen": () => simple("sharpen"),
  "anime4k-s": () => anime4k("small", 4),
  "anime4k-m": () => anime4k("medium", 8),
  "anime4k-l": () => anime4k("large", 9),
  // michadame's "Sony Trinitron (90s)", "Sony PVM-20M2 (90s Broadcast)" and "Budget 13" Dorm TV (Early 90s RF)".
  "crt-trinitron": () => halo({ brightboost: 1.2, brightboost1: 1.4, beam_min: 1.7, beam_max: 1.15, beam_size: 1.35, h_sharp: 3.5, vibrance: 1.1, glow: 0.38, bloom: 0.52, halation: 0.18, shadow_mask: 6, masksize: 1, maskstr: 0.42, mcut: 0.72, slotmask: 0, slotmask1: 0, double_slot: 3, smoothmask: 0.95, halo_intensity: 0.85, corner_size: 0.025, curvature: 1 }),
  "crt-pvm": () => halo({ brightboost: 1.18, brightboost1: 1.32, beam_min: 2.05, beam_max: 1.25, beam_size: 1.15, h_sharp: 6.5, vibrance: 1.05, glow: 0.22, bloom: 0.35, halation: 0.08, shadow_mask: 6, masksize: 1, maskstr: 0.35, mcut: 0.65, slotmask: 0, slotmask1: 0, double_slot: 3, smoothmask: 0.75, halo_intensity: 0.55, corner_size: 0.018, curvature: 1 }),
  "crt-dorm": () => halo({ brightboost: 1.38, brightboost1: 1.6, beam_min: 1.35, beam_max: 0.85, beam_size: 1.65, h_sharp: 1.45, vibrance: 0.96, glow: 0.52, bloom: 0.65, halation: 0.28, shadow_mask: 6, masksize: 1, maskstr: 0.48, mcut: 0.78, slotmask: 0.32, slotmask1: 0.45, double_slot: 3, smoothmask: 1.15, halo_intensity: 1.05, corner_size: 0.045, curvature: 1 }),
};

const VERTEX = `#version 300 es
layout(location = 0) in vec2 a_pos;
out vec2 v_tc;
void main() {
    gl_Position = vec4(a_pos, 0.0, 1.0);
    v_tc = a_pos * 0.5 + 0.5;
}`;

/** The picture, onto the canvas. A frame's first row is its top; the canvas's is its bottom. */
const present: Pass = {
  shader: `#version 300 es
precision highp float;
in vec2 v_tc;
out vec4 out_color;
uniform sampler2D input_tex;
void main() {
    out_color = vec4(texture(input_tex, vec2(v_tc.x, 1.0 - v_tc.y)).rgb, 1.0);
}`,
  binds: { input_tex: "MAIN" },
  save: null,
  size: (s) => s.out,
};

export interface Renderer {
  /** Draws a frame through a filter, to fill the canvas. `preview` is for the panel's small tiles. */
  draw(frame: TexImageSource, width: number, height: number, filter: string, preview?: boolean): void;
  dispose(): void;
}

export function createRenderer(canvas: HTMLCanvasElement): Renderer {
  const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, depth: false })!;
  // The passes between keep half-float textures: Anime4K's hold negative numbers.
  gl.getExtension("EXT_color_buffer_float");

  gl.bindVertexArray(gl.createVertexArray());
  gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  gl.enableVertexAttribArray(0);
  gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

  function texture(): WebGLTexture {
    const tex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return tex;
  }
  const frameTex = texture();

  function compile(type: number, source: string): WebGLShader {
    const shader = gl.createShader(type)!;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(shader) ?? "A filter’s shader didn’t compile");
    return shader;
  }
  const vertex = compile(gl.VERTEX_SHADER, VERTEX);

  interface Program {
    program: WebGLProgram;
    uniforms: { name: string; type: number; at: WebGLUniformLocation }[];
  }
  const programs = new Map<string, Program>();
  function program(source: string): Program {
    let p = programs.get(source);
    if (p) return p;
    const made = gl.createProgram();
    gl.attachShader(made, vertex);
    gl.attachShader(made, compile(gl.FRAGMENT_SHADER, source));
    gl.linkProgram(made);
    if (!gl.getProgramParameter(made, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(made) ?? "A filter’s shader didn’t link");
    p = { program: made, uniforms: [] };
    for (let i = 0; i < gl.getProgramParameter(made, gl.ACTIVE_UNIFORMS); i++) {
      const u = gl.getActiveUniform(made, i)!;
      p.uniforms.push({ name: u.name, type: u.type, at: gl.getUniformLocation(made, u.name)! });
    }
    programs.set(source, p);
    return p;
  }

  interface Target {
    tex: WebGLTexture;
    fbo: WebGLFramebuffer;
    w: number;
    h: number;
  }
  const targets = new Map<string, Target>();
  function target(key: string, w: number, h: number): Target {
    let t = targets.get(key);
    if (!t) {
      t = { tex: texture(), fbo: gl.createFramebuffer(), w: 0, h: 0 };
      gl.bindFramebuffer(gl.FRAMEBUFFER, t.fbo);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, t.tex, 0);
      targets.set(key, t);
    }
    if (t.w !== w || t.h !== h) {
      gl.bindTexture(gl.TEXTURE_2D, t.tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA16F, w, h, 0, gl.RGBA, gl.HALF_FLOAT, null);
      t.w = w;
      t.h = h;
    }
    return t;
  }

  const built = new Map<string, Pass[]>();

  return {
    draw(frame, width, height, filter, preview = false) {
      let passes = built.get(filter);
      if (!passes) {
        passes = chains[filter]();
        if (passes[passes.length - 1].save !== null) passes.push(present);
        built.set(filter, passes);
      }
      const sizes: Sizes = {
        source: [width, height],
        main: [width, height],
        out: [canvas.width, canvas.height],
        // michadame gives its televisions a picture of at least 1080 lines.
        work: preview || height >= 1080 ? [width, height] : [Math.round((width * 1080) / height), 1080],
      };
      gl.bindTexture(gl.TEXTURE_2D, frameTex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame);
      const textures = new Map<string, WebGLTexture>([["MAIN", frameTex]]);
      passes.forEach((pass, i) => {
        const [w, h] = pass.size(sizes);
        // A pass that reads MAIN and draws it gets a texture of its own to draw to.
        const to = pass.save === null ? null : target(`${i}:${pass.save}`, w, h);
        const p = program(pass.shader);
        gl.useProgram(p.program);
        const values = pass.uniforms?.(sizes) ?? {};
        let unit = 0;
        for (const u of p.uniforms) {
          const v = values[u.name];
          if (u.type === gl.SAMPLER_2D) {
            gl.activeTexture(gl.TEXTURE0 + unit);
            gl.bindTexture(gl.TEXTURE_2D, textures.get(pass.binds[u.name])!);
            gl.uniform1i(u.at, unit++);
          } else if (v === undefined) continue;
          else if (u.type === gl.INT) gl.uniform1i(u.at, v as number);
          else if (typeof v === "number") gl.uniform1f(u.at, v);
          else if (v.length === 2) gl.uniform2fv(u.at, v);
          else if (v.length === 3) gl.uniform3fv(u.at, v);
          else gl.uniform4fv(u.at, v);
        }
        gl.bindFramebuffer(gl.FRAMEBUFFER, to?.fbo ?? null);
        gl.viewport(0, 0, w, h);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
        if (to && pass.save) {
          textures.set(pass.save, to.tex);
          if (pass.save === "MAIN") sizes.main = [w, h];
        }
      });
    },
    dispose() {
      gl.getExtension("WEBGL_lose_context")?.loseContext();
    },
  };
}

const TILE = 160;

/** Draws each filter over the middle of the video's frame, small, onto the panel's tiles:
 *  one tile a frame, so opening the panel doesn't hold up the picture. Returns how to stop. */
export function drawTiles(video: HTMLVideoElement, tiles: Record<string, HTMLCanvasElement>): () => void {
  const filtered = document.createElement("canvas");
  filtered.width = filtered.height = TILE;
  const renderer = createRenderer(filtered);
  const middle = document.createElement("canvas");
  const side = Math.min(video.videoWidth, video.videoHeight);
  let next = 0;
  let frame = 0;
  const step = () => {
    const filter = filters[next++];
    if (!filter) return renderer.dispose();
    // Small enough that a filter's work shows at a tile's size: a television's scanlines need fewer lines still.
    const lines = filter.id.startsWith("crt-") ? 54 : 80;
    middle.width = middle.height = lines;
    const small = middle.getContext("2d")!;
    small.imageSmoothingQuality = "high";
    small.drawImage(video, (video.videoWidth - side) / 2, (video.videoHeight - side) / 2, side, side, 0, 0, lines, lines);
    const tile = tiles[filter.id].getContext("2d")!;
    if (filter.id === "none") tile.drawImage(middle, 0, 0, TILE, TILE);
    else {
      renderer.draw(middle, lines, lines, filter.id, true);
      tile.drawImage(filtered, 0, 0);
    }
    frame = requestAnimationFrame(step);
  };
  step();
  return () => {
    cancelAnimationFrame(frame);
    renderer.dispose();
  };
}
