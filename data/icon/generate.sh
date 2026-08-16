#!/bin/bash
# Regenerate the Gopher65 icon set from the master gopher65.png.
set -e
magick gopher65.png -resize 512x512 gopher65_512x512.png
magick gopher65.png -resize 256x256 gopher65_256x256.png
magick gopher65.png -resize 128x128 gopher65_128x128.png
magick gopher65.png -resize 256x256 icon.ico
