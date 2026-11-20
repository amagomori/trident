import unittest
from pathlib import Path

from PIL import ImageChops

from wipe import fit, frame, load


class WipeTest(unittest.TestCase):
    def setUp(self):
        here = Path(__file__).parent
        self.before, self.after = (load(here / name) for name in ("before.png", "after.png"))

    def test_common_scale(self):
        pair = (self.before, self.after)
        self.assertEqual(fit(pair, (2000, 2000)), pair)
        small = fit(pair, (320, 500))
        self.assertEqual([im.size for im in small], [(320, 96)] * 2)
        with self.assertRaisesRegex(ValueError, "identical pixel dimensions"):
            fit((self.before, small[1]), (320, 500))

    def test_split_uses_the_same_coordinates(self):
        a, b = self.before, self.after
        diff = ImageChops.difference(a.convert("RGB"), b.convert("RGB")).getbbox()
        self.assertIsNotNone(diff)
        cut = (diff[0] + diff[2]) // 2
        wipe = frame(a, b, cut)
        for region, original in [((0, 0, cut, a.height), a),
                                 ((cut + 1, 0, a.width, a.height), b)]:
            self.assertEqual(wipe.crop(region).tobytes(), original.crop(region).tobytes())
        self.assertEqual(wipe.getpixel((cut, 0)), (255, 0, 255, 255))
        self.assertEqual(frame(a, b, 0).tobytes(), b.tobytes())
        self.assertEqual(frame(a, b, a.width).tobytes(), a.tobytes())


if __name__ == "__main__":
    unittest.main()
