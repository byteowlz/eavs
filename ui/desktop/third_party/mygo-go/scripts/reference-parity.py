import json, pathlib, importlib.util, importlib.machinery, sys
sys.dont_write_bytecode = True
root=pathlib.Path(__file__).resolve().parent.parent
loader=importlib.machinery.SourceFileLoader("studio_color",str(root/"reference/studio-color.py.txt"))
spec=importlib.util.spec_from_loader("studio_color",loader)
color=importlib.util.module_from_spec(spec)
spec.loader.exec_module(color)
roles=json.loads((root/"data/roles.json").read_text())
cases={}
for path in sorted((root/"data/schemes").glob("*.json")):
    scheme=json.loads(path.read_text())
    slots={k:color.parse_color(v) for k,v in scheme["slots"].items()}
    if scheme["system"]=="base16":
        recipes=[("base10","base00",0.18,False),("base11","base00",0.34,False),("base12","base08",0.22,True),("base13","base0A",0.22,True),("base14","base0B",0.22,True),("base15","base0C",0.22,True),("base16","base0D",0.22,True),("base17","base0E",0.22,True)]
        for target,source,amount,light in recipes:
            if target not in slots:
                slots[target]=slots[source].mix(color.WHITE if light else color.BLACK,amount)
    resolved={role:color.parse_color(scheme.get("overrides",{}).get("--"+role,slots[slot].to_hex())).to_hex() for role,slot in roles.items()}
    cases[scheme["id"]]={"slots":{k:v.to_hex() for k,v in slots.items()},"roles":resolved}
(root/"testdata/parity.json").write_text(json.dumps(cases,indent=2)+"\n")
