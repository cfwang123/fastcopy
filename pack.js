(function(){
	var fs = require('fs');
	var path = require('path');
	var cp = require('child_process');
	var t = {
		run: run
	};
	t.run();
	function run(){
		var root = __dirname;
		var version = readVersion(root);
		var archiveName = 'fastcopy_' + version + '.7z';
		var releaseDir = path.join(root, 'release');
		var archivePath = path.join(releaseDir, archiveName);
		var exeName = 'fastcopy.exe';
		var builtExe = path.join(root, 'target', 'release', exeName);
		var sevenZip = find7z();
		mkdirp(releaseDir);
		tryKill(exeName);
		runCmd('cargo', ['build', '--release'], root);
		if (!fs.existsSync(builtExe))
			fail('missing ' + builtExe);
		if (fs.existsSync(archivePath))
			fs.unlinkSync(archivePath);
		runCmd(sevenZip, ['a', '-t7z', '-mx=9', archivePath, exeName], path.join(root, 'target', 'release'));
		if (!fs.existsSync(archivePath))
			fail('archive not created: ' + archivePath);
		console.log(archivePath);
	}
	function readVersion(root){
		var text = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
		var match = text.match(/^version\s*=\s*"([^"]+)"/m);
		if (!match)
			fail('version not found in Cargo.toml');
		return match[1];
	}
	function find7z(){
		var candidates = [
			'7z',
			'7z.exe',
			path.join(process.env['ProgramFiles'] || 'C:\\Program Files', '7-Zip', '7z.exe'),
			path.join(process.env['ProgramFiles(x86)'] || 'C:\\Program Files (x86)', '7-Zip', '7z.exe')
		];
		for (var i = 0; i < candidates.length; i++){
			var cmd = candidates[i];
			if (cmd.indexOf('\\') >= 0 || cmd.indexOf('/') >= 0){
				if (fs.existsSync(cmd))
					return cmd;
				continue;
			}
			var probe = cp.spawnSync(cmd, [], { encoding: 'utf8' });
			if (probe.error)
				continue;
			return cmd;
		}
		fail('7z not found');
	}
	function tryKill(exeName){
		cp.spawnSync('taskkill', ['/F', '/IM', exeName], { encoding: 'utf8', windowsHide: true });
	}
	function runCmd(command, args, cwd){
		var result = cp.spawnSync(command, args, {
			cwd: cwd,
			stdio: 'inherit',
			windowsHide: true
		});
		if (result.error)
			fail(result.error.message);
		if (result.status !== 0)
			fail(command + ' exited ' + result.status);
	}
	function mkdirp(dir){
		fs.mkdirSync(dir, { recursive: true });
	}
	function fail(message){
		console.error(message);
		process.exit(1);
	}
})();
