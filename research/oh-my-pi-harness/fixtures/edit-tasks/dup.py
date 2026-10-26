"""Lookup helpers."""


def find_user(users, name):
    for user in users:
        if user["name"] == name:
            return user
    return None


def find_group(groups, name):
    for group in groups:
        if group["name"] == name:
            return group
    return None


def find_role(roles, name):
    for role in roles:
        if role["name"] == name:
            return role
    return None
