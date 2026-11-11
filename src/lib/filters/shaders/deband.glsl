#version 300 es
precision highp float;
precision highp sampler2D;
// Debanding, the way mpv's deband does it: where the picture around a pixel is
// almost its colour, the pixel takes the average, and a little grain keeps an
// 8-bit screen from banding it again.
in vec2 v_tc;
out vec4 out_color;
uniform sampler2D input_tex;
uniform float seed;

const int ITERATIONS = 2;
const float THRESHOLD = 48.0;
const float RANGE = 16.0;
const float GRAIN = 24.0;

float mod289(float x) { return x - floor(x / 289.0) * 289.0; }
float permute(float x) { return mod289((34.0 * x + 1.0) * x); }
float rand(float x) { return fract(x / 41.0); }

vec4 average(float range, inout float h) {
    float dist = rand(h) * range;
    h = permute(h);
    float dir = rand(h) * 6.2831853;
    h = permute(h);
    vec2 o = dist * vec2(cos(dir), sin(dir)) / vec2(textureSize(input_tex, 0));
    return 0.25 * (texture(input_tex, v_tc + o) + texture(input_tex, v_tc + vec2(-o.y, o.x))
        + texture(input_tex, v_tc - o) + texture(input_tex, v_tc + vec2(o.y, -o.x)));
}

void main() {
    vec3 m = vec3(v_tc, seed) + vec3(1.0);
    float h = permute(permute(permute(m.x) + m.y) + m.z);
    vec4 color = texture(input_tex, v_tc);
    for (int i = 1; i <= ITERATIONS; i++) {
        vec4 avg = average(float(i) * RANGE, h);
        color = mix(avg, color, greaterThan(abs(color - avg), vec4(THRESHOLD / (float(i) * 16384.0))));
    }
    vec3 noise = vec3(rand(h));
    h = permute(h);
    noise.g = rand(h);
    h = permute(h);
    noise.b = rand(h);
    out_color = vec4(color.rgb + (GRAIN / 8192.0) * (noise - 0.5), 1.0);
}
