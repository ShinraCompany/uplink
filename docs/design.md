Design keeping this in mind

* Swappable collectors and serializers
* Persistence with priority
* Graceful shutdown
* Disable and enable a channel of collector dynamically
* Disable and enable a collector dynamically
* Sampling channel data
* System metrics like memory, cpu, network io etc
* Self Instrumentation..Incoming bandwidth, Outgoing bandwidth, Disk backlog etc
* Self errors should also be reported to the cloud


Several collector types
-----------------------

* produces data of multiple channels like journal
* produces data of single channel. like can and system monitor

All the collectors generate types which satisfy `Packable` trait so that
serializer can identify channels and serialize


We should be able to spawn several collector types simultaneously. e.g
journal, can and action collectors should run in our case and write
data to a single channel so that we don't have to use select!


Actions
------

Actions are requests from cloud over mqtt or locally from grpc to
perform following operations

* Receive system actions like reboot, shutdown, ota etc
* Actions for stopping collector or a given channel of a collector
* Actions should be able to start a new collector so that we can
  remotely start a collector. So actions is the entry point in the main
  thread
* Actions have controller tx to each collector for remote actions. e.g.
  stop collecting data of a given channel, shutdown the collector, collection interval

Collector
-------

* Collects data from hardware (or journal like)
* Creates packable data out of collected data and send it over channel
* Handle controller notifications..stop a channel, slowdown a channel,
  stop the collector


Self Instrumentation
--------

* Critical errors that happened in a collector
* Channel incoming data throughput
* Channel outgoing data throughput
* Total incoming and outgoing data throughput
* Number of files in disk
* Total backlog size
* Rougue data. Receiving data of invalid channels. Report first incident
  and frequency for a given time period
* This can be a hashmap 

System Monitor
---------

* Cpu usage total and per process
* Memory usage total and per process
* Disk information
* Can be a hashmap


Design
------

As per our current use case, a collector should be able to do following
tasks

* Collect data from hardware or network
* Handle actions notification to shutdown and restart collection of a
  single channel or the collector itself. so a controller notification
  should go to a given collector

Each collector should sit into uplink's collector lifecycle. 


TODO
-----

-[ ] Validate collector for a predefined set of channels and report
rogue data. Rogue data should be part of reporting stack

-[ ] Actions status vs device status as combined vs independent
     * Combined json for mutually exclusive things isn't great
     * Create dummy "client" key along with "bytebeam" key during status
