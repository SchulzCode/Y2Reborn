# Reborn audio fixtures

The generated fixtures are deterministic 440 Hz, ramp, artwork, metadata and
corruption inputs produced locally with FFmpeg 9.0.1 by
`tools/build/fixtures.py`. Their hashes are recorded in `manifest.json`.

`ape-silence.ape` and `wavpack-silence.wv` are small decoder specimens because
the host FFmpeg build does not provide APE or WavPack encoders. They were
retrieved on 2026-09-18 from the Wavecor test-disc files
[`Track73.ape`](https://www.wavecor.co.uk/TestDisc/Track73.ape) and
[`Track73.wv`](https://www.wavecor.co.uk/TestDisc/Track73.wv). They remain
decoder test inputs; the generated-fixture CC0 statement does not apply to
these two files.
