# FrostBuild site and mark

This directory is the source for the official static site at
<https://hjosugi.github.io/frost-build/>. It has no runtime dependencies and
uses relative asset URLs so it works at the repository's Pages base path.

Brand assets:

- `assets/frostbuild-mark.png` — 1254 px transparent master;
- `assets/frostbuild-mark-512.png` — navigation, README and social preview;
- `assets/apple-touch-icon.png` — 180 px touch icon;
- `favicon.png` — 32 px browser icon.

The master was generated with OpenAI's built-in image generation on 28 July
2026, then its solid chroma-key background was converted to alpha and the
smaller assets were derived from that project-bound copy. Prompt:

> Create a crisp modern brand mark for FrostBuild: a symmetrical six-point
> frost crystal whose branches also read as a dependency graph, with small
> hexagonal nodes connected by clean white lines. Use deep navy, electric cyan
> and pale ice-blue geometric facets, no text, no letters, no mockup, centered
> with generous padding, on a perfectly flat solid chroma-key magenta
> background (#FF00FF).

`.github/workflows/pages.yml` publishes this exact directory through
GitHub's official SHA-pinned Pages actions.
