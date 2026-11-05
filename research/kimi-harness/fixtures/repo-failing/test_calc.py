import unittest
from calc import mean, clamp


class T(unittest.TestCase):
    def test_mean(self):
        self.assertEqual(mean([1, 2, 3]), 2)

    def test_clamp(self):
        self.assertEqual(clamp(5, 0, 3), 3)


if __name__ == "__main__":
    unittest.main()
