+++
names = ["tyrone", "sluggy", "bushcraft"]

[generate_from]
selector = "/names"
filename_format = "names-[]"
+++
{% set entry = entry_path | entry -%}
{% for name in entry.names -%}
{{name}}{% if name == entry.generate %} [current]{% endif -%}
{% if not loop.last -%}{{ cr() }}{% endif -%}
{% endfor -%}
