import sys, json, subprocess
sys.path.insert(0, sys.argv[1])
from markdown_it import MarkdownIt
md = MarkdownIt("commonmark")
out = []
for repo in ["stack-agent", "appboundry", "authboundry", "pax", "synapse"]:
    root = f"/home/user/{repo}/"
    files = subprocess.run(["git", "-C", root, "ls-files", "*.md"], capture_output=True, text=True).stdout.split()
    for f in files:
        try: text = open(root + f, encoding="utf-8").read()
        except Exception: continue
        links, heads, tokens = [], [], md.parse(text)
        for i, t in enumerate(tokens):
            if t.type == "heading_open": heads.append(tokens[i + 1].content)
            for c in (t.children or []):
                if c.type == "link_open": links.append(c.attrs.get("href", ""))
        out.append({"path": repo + "/" + f, "abs": root + f, "chars": len(text), "links": links, "headings": heads})
json.dump(out, open(sys.argv[2], "w"))
print(len(out))
