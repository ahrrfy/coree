// Rhai Hierarchical Scene DOM batch recolor probe for T4-W4-SCENE10K-BENCH (W4 ADD-11)
let p0 = doc.pages[0];
let l0 = p0.layers[0];
let shapes = l0.shapes;
if shapes.len() < 10000 {
    throw "expected >= 10000 shapes in layer 0, got " + shapes.len();
}
log("DOM_SHAPES=" + shapes.len());
let target_ids = [];
for i in 0..10 {
    target_ids.push(shapes[i].id);
}
set_fill_hex(target_ids, "#112233");
