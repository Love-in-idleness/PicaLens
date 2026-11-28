// FidelityFX Contrast Adaptive Sharpening (CAS)
// Adapted from AMD FidelityFX-CAS (MIT License).
// Works both as a 1:1 post-sharpening pass and as a lightweight upscaler.

uniform float sharpness;

vec3 fetchCas(vec2 uv) {
    return texture(inputImage, clamp(uv, vec2(0.0), vec2(1.0))).rgb;
}

void main() {
    vec2 sourceSize = vec2(textureSize(inputImage, 0));
    vec2 pt = 1.0 / sourceSize;
    vec2 uv = gl_FragCoord.xy / outputSize;

    vec3 a = fetchCas(uv + vec2(-pt.x, -pt.y));
    vec3 b = fetchCas(uv + vec2(0.0,   -pt.y));
    vec3 c = fetchCas(uv + vec2( pt.x, -pt.y));
    vec3 d = fetchCas(uv + vec2(-pt.x,  0.0));
    vec3 e = fetchCas(uv);
    vec3 f = fetchCas(uv + vec2( pt.x,  0.0));
    vec3 g = fetchCas(uv + vec2(-pt.x,  pt.y));
    vec3 h = fetchCas(uv + vec2(0.0,    pt.y));
    vec3 i = fetchCas(uv + vec2( pt.x,  pt.y));

    vec3 mn = min(min(min(d, e), min(f, b)), h);
    vec3 mn2 = min(mn, min(min(a, c), min(g, i)));
    mn += mn2;

    vec3 mx = max(max(max(d, e), max(f, b)), h);
    vec3 mx2 = max(mx, max(max(a, c), max(g, i)));
    mx += mx2;

    vec3 rcpM = 1.0 / max(mx, vec3(1e-5));
    vec3 amp = clamp(min(mn, 2.0 - mx) * rcpM, 0.0, 1.0);
    amp = sqrt(amp);

    float peak = -1.0 / mix(8.0, 5.0, clamp(sharpness, 0.0, 1.0));
    vec3 w = amp * peak;
    vec3 rcpW = 1.0 / (1.0 + 4.0 * w);
    vec3 outRgb = clamp(((b + d + f + h) * w + e) * rcpW, 0.0, 1.0);
    outputColor = vec4(outRgb, 1.0);
}
