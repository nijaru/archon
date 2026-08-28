from pathlib import Path

path = Path("crates/kernel/src/select.rs")
text = path.read_text()

old_call = '''        return select_spread(
            graph,
            request,
            already,
            need,
            &ctx,
            candidates,
            count,
            topology_trace,
        );
'''
new_call = '''        return select_spread(already, need, &ctx, candidates, count, topology_trace);
'''
if text.count(old_call) != 1:
    raise RuntimeError("generated select_spread call changed")
text = text.replace(old_call, new_call, 1)

old_sig = '''fn select_spread(
    graph: &Graph,
    request: &Request,
    already: &[Vec<Claim>],
    need: &Need,
    ctx: &ScoreCtx<'_>,
    candidates: Vec<NodeId>,
    count: u64,
    topology_trace: &mut TopologyTrace,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
'''
new_sig = '''fn select_spread(
    already: &[Vec<Claim>],
    need: &Need,
    ctx: &ScoreCtx<'_>,
    candidates: Vec<NodeId>,
    count: u64,
    topology_trace: &mut TopologyTrace,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
    let graph = ctx.graph;
    let request = ctx.request;
'''
if text.count(old_sig) != 1:
    raise RuntimeError("generated select_spread signature changed")
path.write_text(text.replace(old_sig, new_sig, 1))
