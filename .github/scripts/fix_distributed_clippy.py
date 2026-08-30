from pathlib import Path

path = Path('crates/node/tests/distributed_activation.rs')
text = path.read_text()
text = text.replace(
'''    Prepare {\n        machine: String,\n    },''',
'''    Prepare,'''
)
text = text.replace(
'''                    .push(Event::Prepare {\n                        machine: self.machine.clone(),\n                    });''',
'''                    .push(Event::Prepare);'''
)
text = text.replace(
'''            .filter(|event| matches!(event, Event::Prepare { .. }))''',
'''            .filter(|event| matches!(event, Event::Prepare))'''
)
path.write_text(text)
