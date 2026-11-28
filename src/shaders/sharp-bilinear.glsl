// Sharp-Bilinear pixel-art upscaler
// Adapted from libretro common-shaders (sharp-bilinear.cg, Public Domain).

void main() {
    vec2 sourceSize = vec2(textureSize(inputImage, 0));
    vec2 scale = max(floor(outputSize / sourceSize), vec2(1.0));
    vec2 pos = gl_FragCoord.xy / outputSize;
    vec2 texel = pos * sourceSize;
    vec2 texelFloored = floor(texel);
    vec2 s = fract(texel);
    vec2 regionRange = 0.5 - 0.5 / scale;
    vec2 centerDist = s - 0.5;
    vec2 f = (centerDist - clamp(centerDist, -regionRange, regionRange)) * scale + 0.5;
    vec2 uv = (texelFloored + f) / sourceSize;
    outputColor = vec4(texture(inputImage, uv).rgb, 1.0);
}
