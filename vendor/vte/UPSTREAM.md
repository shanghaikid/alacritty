This copy is based on https://github.com/shanghaikid/vte at
7cfdf88e709ab444ed9d69b6bd16c02d7c092839 (vte 0.15.0).

Local extension: dispatch OSC 133 A/B/C/D markers through Handler::shell_marker,
including optional validated decimal exit status. The existing shell_prompt
callback remains compatible. tests/shell_markers.rs covers BEL/ST termination,
fragmented input, optional parameters, and malformed status fields.

The original Apache-2.0 / MIT licenses are preserved alongside the source.
