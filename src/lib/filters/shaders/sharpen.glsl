#version 300 es
precision highp float;
precision highp sampler2D;
// Sharpening by AMD's contrast adaptive sharpening (FidelityFX CAS), on a pixel
// and its four neighbours: flat areas and edges that are already hard get less.
in vec2 v_tc;
out vec4 out_color;
uniform sampler2D input_tex;

const float SHARPNESS = 0.6;

void main() {
    vec2 px = 1.0 / vec2(textureSize(input_tex, 0));
    vec3 e = texture(input_tex, v_tc).rgb;
    vec3 b = texture(input_tex, v_tc - vec2(0.0, px.y)).rgb;
    vec3 d = texture(input_tex, v_tc - vec2(px.x, 0.0)).rgb;
    vec3 f = texture(input_tex, v_tc + vec2(px.x, 0.0)).rgb;
    vec3 h = texture(input_tex, v_tc + vec2(0.0, px.y)).rgb;
    vec3 lo = min(min(min(b, d), min(f, h)), e);
    vec3 hi = max(max(max(b, d), max(f, h)), e);
    vec3 amp = sqrt(clamp(min(lo, 1.0 - hi) / max(hi, 1e-4), 0.0, 1.0));
    vec3 w = amp * mix(-0.125, -0.2, SHARPNESS);
    out_color = vec4(clamp((w * (b + d + f + h) + e) / (1.0 + 4.0 * w), 0.0, 1.0), 1.0);
}
