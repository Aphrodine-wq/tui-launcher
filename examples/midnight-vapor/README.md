# Theme packs

A theme pack is just this folder. Share one by zipping the folder; install
one by dropping it into `~/.config/tui-launcher/themes/`. Select it in the
launcher under **Settings → Appearance → Theme pack**.

## Layout

```
themes/<pack-name>/
├── theme.toml        # colors, background, sounds — every field optional
└── icons/            # per-category icons, any subset
    ├── settings.svg  # svg, png, webp, or jpg
    ├── extras.svg
    ├── photo.svg
    ├── music.svg
    ├── video.svg
    ├── game.svg
    └── network.svg
```

Anything a pack does not provide falls back to the built-in look: missing
icons keep the drawn vector icons, a missing accent keeps the palette
chosen under Wave accent, a missing gradient keeps the monthly colors.

Icons render best as square-ish artwork on a transparent background; they
are tinted/faded by the interface, so white or light shapes work best.
Sounds are WAV files played at the interface volume.
