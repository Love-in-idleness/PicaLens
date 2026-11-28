// Stochastic Debanding post-processing shader
// Adapted from haasn/mpv deband.glsl (GPLv3 / LGPLv2.1+).

float mod289(float x) { return x - floor(x / 289.0) * 289.0; }
float permute(float x) { return mod289((34.0 * x + 1.0) * x); }
float prng(float x) { return fract(x / 41.0); }

vec3 sampleAverage(vec2 pos, vec2 pt, float r, inout float h) {
    float dist = prng(h) * r;          h = permute(h);
    float dir  = prng(h) * 6.2831853;  h = permute(h);
    vec2 o = vec2(cos(dir), sin(dir)) * pt * dist;
    vec3 c0 = texture(inputImage, pos + vec2( o.x,  o.y)).rgb;
    vec3 c1 = texture(inputImage, pos + vec2(-o.y,  o.x)).rgb;
    vec3 c2 = texture(inputImage, pos + vec2(-o.x, -o.y)).rgb;
    vec3 c3 = texture(inputImage, pos + vec2( o.y, -o.x)).rgb;
    return 0.25 * (c0 + c1 + c2 + c3);
}

void main() {
    vec2 sourceSize = vec2(textureSize(inputImage, 0));
    vec2 pt = 1.0 / sourceSize;
    vec2 pos = gl_FragCoord.xy / outputSize;

    vec3 m = vec3(pos, 4.6);
    float h = permute(permute(permute(m.x) + m.y) + m.z);

    vec3 col = texture(inputImage, pos).rgb;
    const float threshold = 64.0;
    const float range = 8.0;
    const float grain = 32.0;

    for (int i = 1; i <= 4; ++i) {
        float fi = float(i);
        vec3 avg = sampleAverage(pos, pt, fi * range, h);
        vec3 diff = abs(col - avg);
        vec3 thres = vec3(threshold / (fi * 16384.0));
        col = mix(avg, col, step(thres, diff));
    }

    vec3 noise;
    noise.x = prng(h); h = permute(h);
    noise.y = prng(h); h = permute(h);
    noise.z = prng(h);
    col = clamp(col + (grain / 8192.0) * (noise - 0.5), 0.0, 1.0);
    outputColor = vec4(col, 1.0);
}
