-- Records which target built it, so a reader can tell prod's `a` from dev's.
select '{{ target.name }}' as built_in
