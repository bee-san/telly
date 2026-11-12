#version 300 es
precision highp float;
precision highp sampler2D;
// Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved. MIT: see README.md.
// AMD FidelityFX Super Resolution 1's sharpening (RCAS): FsrRcasF from ffx_fsr1.h
// v1.0.2, with its noise detection on (FSR_RCAS_DENOISE), so grain is sharpened
// less. Each pixel and its four neighbours: as sharp as it can be without clipping.
// Sharpness is agyild's mpv port's: 0.2 stops below the most.
in vec2 v_tc;
out vec4 out_color;
uniform sampler2D input_tex;

const float SHARPNESS = 0.2;
// This is set at the limit of providing unnatural results for sharpening.
#define FSR_RCAS_LIMIT (0.25 - (1.0 / 16.0))

float APrxMedRcpF1(float a) {
    float b = uintBitsToFloat(uint(0x7ef19fff) - floatBitsToUint(a));
    return b * (-b * a + 2.0);
}
float AMax3F1(float x, float y, float z) { return max(x, max(y, z)); }
float AMin3F1(float x, float y, float z) { return min(x, min(y, z)); }
vec3 AMax3F3(vec3 x, vec3 y, vec3 z) { return max(x, max(y, z)); }
vec3 AMin3F3(vec3 x, vec3 y, vec3 z) { return min(x, min(y, z)); }

// A texel, as far as the edge. RCAS works out its sharpening for colours from 0 to 1.
vec3 at(ivec2 p) {
    return clamp(texelFetch(input_tex, clamp(p, ivec2(0), textureSize(input_tex, 0) - 1), 0).rgb, 0.0, 1.0);
}

void main() {
    // Algorithm uses minimal 3x3 pixel neighborhood.
    //    b
    //  d e f
    //    h
    ivec2 sp = ivec2(gl_FragCoord.xy);
    vec3 b = at(sp + ivec2(0, -1));
    vec3 d = at(sp + ivec2(-1, 0));
    vec3 e = at(sp);
    vec3 f = at(sp + ivec2(1, 0));
    vec3 h = at(sp + ivec2(0, 1));
    // Luma times 2.
    const vec3 luma = vec3(0.5, 1.0, 0.5);
    float bL = dot(b, luma);
    float dL = dot(d, luma);
    float eL = dot(e, luma);
    float fL = dot(f, luma);
    float hL = dot(h, luma);
    // Noise detection.
    float nz = 0.25 * bL + 0.25 * dL + 0.25 * fL + 0.25 * hL - eL;
    nz = clamp(abs(nz) * APrxMedRcpF1(AMax3F1(AMax3F1(bL, dL, eL), fL, hL) - AMin3F1(AMin3F1(bL, dL, eL), fL, hL)), 0.0, 1.0);
    nz = -0.5 * nz + 1.0;
    // Min and max of ring.
    vec3 mn4 = min(AMin3F3(b, d, f), h);
    vec3 mx4 = max(AMax3F3(b, d, f), h);
    // Immediate constants for peak range.
    vec2 peakC = vec2(1.0, -1.0 * 4.0);
    // Limiters, these need to be high precision RCPs. (Kept off 0 here, for black and white.)
    vec3 hitMin = min(mn4, e) / max(4.0 * mx4, 1e-5);
    vec3 hitMax = (peakC.x - max(mx4, e)) / min(4.0 * mn4 + peakC.y, -1e-5);
    vec3 lobeRGB = max(-hitMin, hitMax);
    float lobe = max(-FSR_RCAS_LIMIT, min(AMax3F1(lobeRGB.r, lobeRGB.g, lobeRGB.b), 0.0)) * exp2(-SHARPNESS);
    // Apply noise removal.
    lobe *= nz;
    // Resolve, which needs the medium precision rcp approximation to avoid visible tonality changes.
    float rcpL = APrxMedRcpF1(4.0 * lobe + 1.0);
    out_color = vec4((lobe * b + lobe * d + lobe * h + lobe * f + e) * rcpL, 1.0);
}
