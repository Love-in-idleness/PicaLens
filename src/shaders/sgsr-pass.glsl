uniform float sharpness;

void main() {
    outputColor = vec4(sampleSgsr1(inputImage, gl_FragCoord.xy / outputSize, sharpness).rgb, 1.0);
}
