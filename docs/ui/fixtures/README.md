# Preview-only fixtures

`album-art.png` is the sample artwork from the owner-supplied Reborn Y2 Simple
UI Asset Pack. It is used only by the host preview rasterizer. It is not linked
into Reborn, installed in the root image, or copied into any music library.

The `reborn-preview` executable owns all sample artists, networks, battery
charging flags and platform observations. It is never installed in a candidate.
Production uses observed services and real embedded/sidecar artwork.
