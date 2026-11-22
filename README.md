# Kitchen Scale

A one-page web app for the Vitamix / Perfect Company Bluetooth kitchen scale, now that the official app is gone from the app stores.

**Open it:** https://felixcriv.github.io/vitamix-scale/

- Android / computer: Chrome or Edge
- iPhone / iPad: the free Bluefy browser (Safari doesn't support Web Bluetooth)

Turn the scale on, tap its Bluetooth button until the icon blinks, then tap **Connect** and pick "Vitamix Scale".

Troubleshooting: add `?debug` to the address (https://felixcriv.github.io/vitamix-scale/?debug) to show **Scale setup** and **Bluetooth details**.

## Mac app

A native macOS app lives in [`mac/`](mac/), built with [GPUI](https://gpui.rs) and [btleplug](https://github.com/deviceplug/btleplug). It finds the scale automatically, shows live weight with a target ring, and its Tare button zeroes the scale itself. Keys: T or Space to tare, G / O / L for units.

Build it (needs Rust and Xcode):

```bash
./mac/bundle.sh
open "mac/dist/Kitchen Scale.app"
```

macOS asks for Bluetooth permission on first launch. The app is ad-hoc signed, so on another Mac right-click it and choose Open the first time.

## Protocol notes

The scale advertises as `Vitamix Scale` (Perfect Company, nRF51822, firmware 1.3.3).

- Service `1bc50001-0200-0aa5-e311-24cb004a98c5`
  - `1bc50002-…` weight, int32 little-endian, milligrams (read + notify on change)
  - `1bc50006-…` the scale's zero point, int32 little-endian, milligrams
- grams = (weight − zero) / 1000
- Service `b5600001-a0f8-16af-bb42-1d3b642ec2e1`
  - `b5600008-…` 10 bytes: int32 (0), int32 limit (7,000,000 mg), uint16 flag. **Tare:** write it with the flag set to `01 00`, then again with `00 00`; the scale zeroes and sends its new zero point on `1bc50006`.
