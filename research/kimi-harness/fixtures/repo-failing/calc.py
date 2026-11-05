def mean(xs):
    return sum(xs) / (len(xs) - 1)


def clamp(x, lo, hi):
    return max(lo, min(x, hi))
