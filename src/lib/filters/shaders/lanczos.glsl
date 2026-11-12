#version 300 es
precision highp float;
precision highp sampler2D;
// Lanczos 3 (a sinc windowed by a sinc three times wider), along one axis: run
// across and then down, as mpv's separable scalers are. Shrinking, the kernel
// widens with it, so fine detail doesn't alias. Growing, the result is kept
// partly between the two nearest texels, as mpv's anti-ringing does, so dark
// lines don't get a pale halo.
in vec2 v_tc;
out vec4 out_color;
uniform sampler2D input_tex;
// 0 across, 1 down.
uniform int axis;
// The size this pass draws at.
uniform vec2 out_size;

const float RADIUS = 3.0;
const float ANTIRING = 0.6;
const float PI = 3.14159265;

float sinc(float x) {
    return abs(x) < 1e-4 ? 1.0 : sin(PI * x) / (PI * x);
}

void main() {
    ivec2 size = textureSize(input_tex, 0);
    float n = float(size[axis]);
    // How much the picture shrinks, if it does.
    float shrink = max(n / out_size[axis], 1.0);
    float pos = v_tc[axis] * n - 0.5;
    float base = floor(pos);
    float f = pos - base;
    int reach = int(ceil(RADIUS * shrink));
    // Along the other axis, this pass draws at the picture's own size.
    ivec2 p = ivec2(gl_FragCoord.xy);
    vec4 sum = vec4(0.0);
    float total = 0.0;
    vec4 lo = vec4(1e9);
    vec4 hi = vec4(-1e9);
    for (int i = 1 - reach; i <= reach; i++) {
        float x = (float(i) - f) / shrink;
        float w = abs(x) < RADIUS ? sinc(x) * sinc(x / RADIUS) : 0.0;
        p[axis] = clamp(int(base) + i, 0, size[axis] - 1);
        vec4 c = texelFetch(input_tex, p, 0);
        sum += w * c;
        total += w;
        if (i == 0 || i == 1) {
            lo = min(lo, c);
            hi = max(hi, c);
        }
    }
    vec4 c = sum / total;
    out_color = shrink > 1.0 ? c : mix(c, clamp(c, lo, hi), ANTIRING);
}
