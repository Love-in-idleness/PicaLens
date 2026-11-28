uniform float sharpness;

vec4 FsrRcasLoadF(ivec2 p) {
    return texelFetch(inputImage, clamp(p, ivec2(0), textureSize(inputImage, 0) - 1), 0);
}
void FsrRcasInputF(inout float r, inout float g, inout float b) {}
void main() {
    vec3 color;
    // sharpness is 0.0 (softest) to 1.0 (sharpest); 0.8 matches Gamescope's default 0.2 stops.
    float stops = (1.0 - clamp(sharpness, 0.0, 1.0)) * 1.0;
    FsrRcasF(color.r, color.g, color.b, uvec2(gl_FragCoord.xy), uvec4(floatBitsToUint(exp2(-stops))));
    outputColor = vec4(color, 1.0);
}
