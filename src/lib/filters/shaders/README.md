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

`deband.glsl` follows [mpv](https://mpv.io)'s deband filter, and `sharpen.glsl`
AMD's FidelityFX contrast adaptive sharpening; both are written for Telly.
