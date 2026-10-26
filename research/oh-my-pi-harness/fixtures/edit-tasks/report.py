"""Report formatting."""


def format_row(name, count, width=20):
    label = name.ljust(width)
    return f"{label}{count:>6}"


def format_header(name, count, width=20):
    label = name.upper().ljust(width)
    return f"{label}{'COUNT':>6}"


def format_total(name, count, width=20):
    label = ("total " + name).ljust(width)
    return f"{label}{count:>6}"
