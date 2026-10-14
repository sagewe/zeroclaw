import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import tomllib
import unittest

spec = importlib.util.spec_from_file_location('migration', Path(__file__).resolve().parents[1] / 'migrate_event_watch.py')
migration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(migration)

class MigrationTest(unittest.TestCase):
    def test_preserves_checkpoint_and_other_bindings(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            (root/'config.toml').write_text('''[agents.assistant]
channels = ["wechat.default", "event_watch.mail"]
[channels.event_watch.mail]
enabled = true
source = "fastmail"
mcp_server = "fastmail"
sop = "review-mail"
query = "in:inbox"
poll_interval_secs = 60
[gateway]
require_pairing = true
''')
            db=root/'old.db'
            with sqlite3.connect(db) as c:
                c.execute('create table checkpoint(data text)')
                c.execute('insert into checkpoint values (?)', ('preserved-baseline',))
            backup,target=migration.migrate(root,'mail','mailbox',db)
            config=tomllib.loads((root/'config.toml').read_text())
            self.assertEqual(config['agents']['assistant']['channels'],['wechat.default'])
            self.assertTrue(config['gateway']['require_pairing'])
            self.assertEqual(config['events']['rules']['mail']['sop'],'review-mail')
            self.assertTrue((backup/'config.toml').exists())
            with sqlite3.connect(target/'events.db') as c:self.assertEqual(c.execute('select data from checkpoint').fetchone()[0],'preserved-baseline')
            with self.assertRaises((KeyError,ValueError)):migration.migrate(root,'mail','mailbox',db)

if __name__=='__main__':unittest.main()
