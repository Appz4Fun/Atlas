# Wave 3 B: uploader group pools (Haiku, ~22 calls)
- ngPost sample conf: GROUPS = alt.binaries.test,alt.binaries.misc ; GROUP_POLICY = EACH_POST (values ALL, EACH_POST, EACH_FILE). https://raw.githubusercontent.com/mbruel/ngPost/master/ngPost.conf . Code default _groupPolicy(GROUP_POLICY::ALL) in NgPost.cpp.
- ngPost per-file group pick: PostingJob::_initPosting(): obfuscate && groupPolicyPerFile && nbGroups>1 ? _grpList.at(std::rand() % nbGroups) : _grpList. Uniform random, NOT round-robin. https://raw.githubusercontent.com/mbruel/ngPost/master/src/PostingJob.cpp (high)
- Seed: std::srand(QUuid::createUuid().data1) in NgPost.cpp ctor -> 32-bit seed per run; same rand() stream used for group choice (and per peers, random From). Brute-forceable 2^32 -> predicts group sequence of all files of the run. https://raw.githubusercontent.com/mbruel/ngPost/master/src/NgPost.cpp (high on code)
- Nyuu -g default alt.binaries.test, no per-file random. Postie: groups list, group_policy default each_file (https://postie.kipsilabs.top/docs/configuration), selection code not read.
- Default pools collide (a.b.test), so pool membership is weak unless pool is unusual; weight by rarity.
- Real split example: movie content in a.b.misc, PAR2 in a.b.moovee. https://forums.newsbin.com/viewtopic.php?p=220769 (med)
- No evidence for "obfuscated uploads cluster in a.b.boneless/mom/cores/..." and no indexer using group co-occurrence.
