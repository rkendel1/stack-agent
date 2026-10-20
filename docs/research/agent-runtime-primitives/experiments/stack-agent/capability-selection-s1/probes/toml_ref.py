import sys, json, subprocess, tomllib
out = []
for repo in ["stack-agent", "appboundry", "authboundry", "pax", "synapse"]:
    root = f"/home/user/{repo}/"
    for f in subprocess.run(["git", "-C", root, "ls-files", "*.toml"], capture_output=True, text=True).stdout.split():
        try: ref = tomllib.loads(open(root + f, encoding="utf-8").read())
        except Exception as e: continue
        out.append({"abs": root + f, "ref": ref})
json.dump(out, open(sys.argv[1], "w"), default=str)
print(len(out))
