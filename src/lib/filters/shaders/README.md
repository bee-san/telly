# Filter shaders

`fs_anime4k_*.glsl`, `fs_halo_*.glsl` and `fs_linearize.glsl` are copied unchanged from
[michadame](https://github.com/Morgawr/michadame) (`src/video/shaders`, commit
`6309f03`). They are written for desktop OpenGL; `../index.ts` swaps their
`#version` line for WebGL 2's when it loads them. The three televisions in
`../index.ts` are michadame's built-in Halo presets of the same names.

michadame's own licence says it has none and may be taken freely. Its Anime4K
passes are [Anime4K](https://github.com/bloc97/Anime4K) v3.2's upscaling
networks, made into plain fragment shaders:

> MIT License
>
> Copyright (c) 2019-2021 bloc97
>
> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
> SOFTWARE.

`Anime4K_Restore_CNN_M.glsl`, `Anime4K_Restore_CNN_Soft_M.glsl`,
`Anime4K_Thin_HQ.glsl`, `Anime4K_Darken_HQ.glsl` and
`Anime4K_Denoise_Bilateral_Mode.glsl` are copied unchanged from Anime4K v4.0.1
(`glsl`, commit `4029bf7`), under the same MIT licence. They are mpv shaders:
`mpv()` in `../index.ts` makes each of their passes a WebGL 2 fragment shader
when it loads them, with what mpv gives a pass for each texture it binds
(`MAIN_tex`, `MAIN_pos`, `MAIN_pt`, `MAIN_size`, `MAIN_texOff`) written over a
plain sampler.

`deband.glsl` follows [mpv](https://mpv.io)'s deband filter, and `sharpen.glsl`
AMD's FidelityFX contrast adaptive sharpening; both are written for Telly.

`fsr_easu.glsl` and `fsr_rcas.glsl` are AMD's
[FidelityFX Super Resolution 1](https://github.com/GPUOpen-Effects/FidelityFX-FSR)
v1.0.2: `FsrEasuF` (the upscaler) and `FsrRcasF` (its sharpening) from
`ffx_fsr1.h`, in GLSL on RGB. They follow agyild's
[mpv port](https://gist.github.com/agyild/82219c545228d70c5604f865ce0b0ce5)
(which works on luma only, under the same licence) in reading texels where AMD
gathers them, and take its sharpness of 0.2. RCAS keeps off dividing by 0 in
pure black and white, where AMD's leaves it to the GPU.

> Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.
>
> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in
> all copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
> THE SOFTWARE.

`lanczos.glsl` is written for Telly: Lanczos 3 run across and then down, as
mpv's separable scalers are, with the kernel widened when it shrinks the
picture and, when it grows it, some of mpv's anti-ringing.
