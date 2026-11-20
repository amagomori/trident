# Screenshot wipe (experiment)

From a shell **inside Trident**, outside tmux, run:

```sh
cd experiments/image-wipe
python3 wipe.py before.png after.png
```

Requires Python 3 with curses and Pillow 9.1 or later. Substitute two local PNG
screenshots with identical pixel dimensions (up to 16 megapixels each).
They share one origin and one scale: native size unless the window requires a
smaller fit. Left of the magenta line is **before**; right is **after**.

Use the left/right arrows to move the line in 5% steps. Press **0** for only
after, **1** for only before, or **q** to return to the shell. Try sweeping
across the marker on the **Checks** row: it changes from an empty ring to a
filled dot while its label and the other rows stay still.

Both fixtures were drawn for this experiment; they contain no captured user
data. The script sends a single composite through Trident's existing kitty
graphics support. Nothing is installed into the CLI or written to the input
files. Remove this directory to discard the experiment.

Tests: `python3 -m unittest discover -s .` from this directory.
